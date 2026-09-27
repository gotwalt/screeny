//! `thing`: a right hand, cut off at the wrist, living in a small box - the
//! Addams Family "Thing" the owner asked for, walking on its fingertips and
//! gesturing at the glass. Card 333 built everything the performance needs
//! except the performance itself: the hand ([`hand_rig`]), a clip format
//! and a player that blends between clips by inertialization rather than a
//! cross-fade ([`clip`], [`player`]), contact IK so a planted fingertip
//! does not skate ([`contact_ik`]), a choreographer that picks walks and
//! gestures at random with rests between ([`choreo`]), and the converter
//! that turns the owner's own phone footage into clips
//! (`tools/thing-capture/`).
//!
//! **The render** (card 334) is a GPU raymarched SDF: every bone, knuckle
//! and nail is a capsule or a sphere primitive, smooth-unioned into one
//! surface (`thing.wgsl`'s `scene_sdf`) rather than card 333's supersampled
//! capsule silhouette - "the surface is a real hand, not capsules" (the
//! original card's own words). The box and the camera are this file's own,
//! rescaled for a hand rather than borrowed from `skeletons`' human-scaled
//! ones (`box_scene.rs`'s doc has the reasoning and the numbers); the hand
//! rig's forward kinematics ([`hand_rig::Pose::solve`]) is unchanged.

mod box_scene;
mod choreo;
mod clip;
mod contact_ik;
mod geom;
mod hand_rig;
mod player;

use crate::dither::Dither;
use crate::frame::Frame;
use crate::gpu::{Gpu, Offscreen, COLOR_FORMAT, COMMON_WGSL, DEPTH_FORMAT};
use crate::palette::Palette;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use box_scene::Camera;
use choreo::Choreographer;
use geom::V3;
use hand_rig::{Joints, HAND_H};
use player::Posed;

pub const DEF: PatchDef = PatchDef {
    id: "thing",
    name: "Thing",
    blurb: "GPU, raymarched. A hand cut off at the wrist, living in a lit box: it walks on its fingertips and gestures at the glass.",
    params: PARAMS,
    make,
    // The seed drives the choreographer's own rng (which gesture comes
    // next, how long each rest lasts), so "another one like this" is a
    // real, differently-paced performance.
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    // Defaults calm, matching the autumn set's own ethos ("visual poetry...
    // busyness is a thing we are trying to avoid").
    param("pace", "How fast it moves (lower is slower, dreamier)", 0.4, 2.0, 0.05, 0.8),
    param("light", "How bright the bulb is", 0.5, 1.8, 0.05, 1.0),
    param("glow", "Subsurface warmth and rim glow at the edges", 0.0, 1.5, 0.05, 1.0),
    param("color", "How much colour (0 = grayscale)", 0.0, 1.0, 0.05, 0.35),
];

/// The choreography's own fixed step, matching `skeletons`' `STEP` (card
/// 328's own reasoning applies unchanged: fine enough for a walk cycle,
/// coarse enough that one hand costs nothing).
const STEP: f64 = 1.0 / 30.0;
const CATCHUP: i64 = 300;

struct Thing {
    choreo: Choreographer,
    warped: f64,
    steps: i64,
    live: Option<Option<Live>>,
}

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Thing { choreo: Choreographer::new(seed), warped: 0.0, steps: 0, live: None })
}

impl Thing {
    fn advance(&mut self, ctx: &Ctx) {
        self.warped += ctx.dt.clamp(0.0, 0.25) * f64::from(ctx.get("pace"));
        let target = (self.warped / STEP + 1e-6).floor() as i64;
        self.steps = self.steps.max(target - CATCHUP);
        while self.steps < target {
            self.steps += 1;
            let t = self.steps as f64 * STEP;
            self.choreo.advance(t, STEP as f32);
        }
    }
}

/// Keep the hand's own root inside the box's walkable floor, whatever a
/// clip's own authored coordinates say - a render-time safety net (a
/// placeholder or a converter fit is not guaranteed to stay inside the
/// walls the way `skeletons`' own choreography clamps its actors to), using
/// `box_scene`'s own bounds rather than a second copy of them.
fn keep_in_box(p: V3) -> V3 {
    geom::v3(
        p.x.clamp(-box_scene::WALK_HALF_W, box_scene::WALK_HALF_W),
        p.y,
        p.z.clamp(box_scene::WALK_NEAR_Z, box_scene::WALK_FAR_Z),
    )
}

fn interp(prev: &Posed, curr: &Posed, u: f32, t: f64) -> Posed {
    let mut pose = clip::lerp_pose(&prev.pose, &curr.pose, u);
    pose.thumb.cmc_flex += IDLE_AMPLITUDE * ((t * IDLE_HZ * std::f64::consts::TAU) as f32).sin();
    let pose = pose.clamp();
    Posed {
        root_pos: prev.root_pos.lerp(curr.root_pos, u),
        root_rot: prev.root_rot.slerp(curr.root_rot, u),
        root_frame: geom::Frame::IDENTITY.rotate_q(prev.root_rot.slerp(curr.root_rot, u)),
        pose,
        contacts: curr.contacts,
        clip_name: curr.clip_name.clone(),
    }
}

/// A tiny, ever-present idle tremor on the thumb, a direct function of real
/// engine time (`ctx.t`) rather than the choreographed sim time - see card
/// 333's own doc on why this exists at all (`crates/art/tests/rate.rs`'s
/// "the picture moved" precondition) and why it has to land on the thumb
/// specifically (nothing else is ever un-pinned by contact IK).
const IDLE_HZ: f64 = 0.083;
const IDLE_AMPLITUDE: f32 = 0.15;

// ------------------------------------------------------------- primitives

/// One SDF primitive `thing.wgsl` unions into the scene: a tapered capsule
/// (a bone) or a sphere (a knuckle bump or a nail), tagged with which
/// "chain" it belongs to (so it only ever smooth-unions with its own finger
/// or the palm, never directly with a different finger - see `scene_sdf`'s
/// own doc) and whether it paints as skin or as a nail.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Prim {
    meta: [f32; 4],
    a: [f32; 4],
    b: [f32; 4],
}

const KIND_CAPSULE: f32 = 0.0;
const KIND_SPHERE: f32 = 1.0;
const MAT_SKIN: f32 = 0.0;
const MAT_NAIL: f32 = 1.0;

/// Chain ids `scene_sdf` groups primitives by: the four fingers, the thumb,
/// and the palm/wrist.
const CHAIN_THUMB: f32 = 4.0;
const CHAIN_PALM: f32 = 5.0;

fn capsule(chain: f32, mat: f32, a: V3, ra: f32, b: V3, rb: f32) -> Prim {
    Prim { meta: [KIND_CAPSULE, chain, mat, 0.0], a: [a.x, a.y, a.z, ra], b: [b.x, b.y, b.z, rb] }
}

fn sphere(chain: f32, mat: f32, a: V3, r: f32) -> Prim {
    Prim { meta: [KIND_SPHERE, chain, mat, 0.0], a: [a.x, a.y, a.z, r], b: [0.0; 4] }
}

/// Every primitive the current pose needs, at hand scale `h`. Anatomically
/// the same bones card 333's `bones()` drew (metacarpal, proximal, middle,
/// distal per finger; three thumb bones; the wrist stump), plus knuckle
/// spheres at PIP/DIP and a nail sphere per fingertip - what "smooth unions
/// (palm pad, knuckles, finger taper, nails)" (the card) asks for, built
/// directly from `Joints` rather than through a screen-space projection
/// (the whole point of moving this to a real 3D SDF).
fn gather_primitives(joints: &Joints, h: f32) -> Vec<Prim> {
    let mut out = Vec::with_capacity(40);

    out.push(capsule(CHAIN_PALM, MAT_SKIN, joints.stump(h), hand_rig::STUMP_R * h, joints.wrist, hand_rig::WRIST_R * h));
    // A palm pad: fan capsules between neighbouring metacarpal anchors so
    // the palm reads as one rounded surface rather than four struts
    // meeting at the wrist.
    for i in 0..3 {
        let a = joints.fingers[i][0];
        let b = joints.fingers[i + 1][0];
        out.push(capsule(CHAIN_PALM, MAT_SKIN, a, hand_rig::PALM_R * h * 0.85, b, hand_rig::PALM_R * h * 0.85));
    }

    for i in 0..4 {
        let chain = i as f32;
        let [mcp, pip, dip, tip] = joints.fingers[i];
        let base_r = [0.052, 0.058, 0.055, 0.045][i];
        out.push(capsule(chain, MAT_SKIN, joints.wrist, hand_rig::PALM_R * h, mcp, base_r * h * 1.35));
        out.push(capsule(chain, MAT_SKIN, mcp, base_r * h, pip, base_r * 0.78 * h));
        out.push(capsule(chain, MAT_SKIN, pip, base_r * 0.72 * h, dip, base_r * 0.6 * h));
        out.push(capsule(chain, MAT_SKIN, dip, base_r * 0.55 * h, tip, base_r * 0.4 * h));
        out.push(sphere(chain, MAT_SKIN, pip, 0.030 * h));
        out.push(sphere(chain, MAT_SKIN, dip, 0.026 * h));
        let nail_pos = tip.add(joints.tip_frame[i].fwd.scale(-0.014 * h));
        out.push(sphere(chain, MAT_NAIL, nail_pos, 0.020 * h));
    }

    out.push(capsule(CHAIN_THUMB, MAT_SKIN, joints.wrist, hand_rig::PALM_R * 0.9 * h, joints.thumb[0], 0.05 * h));
    out.push(capsule(CHAIN_THUMB, MAT_SKIN, joints.thumb[0], 0.05 * h, joints.thumb[1], 0.044 * h));
    out.push(capsule(CHAIN_THUMB, MAT_SKIN, joints.thumb[1], 0.044 * h, joints.thumb[2], 0.036 * h));
    out.push(capsule(CHAIN_THUMB, MAT_SKIN, joints.thumb[2], 0.036 * h, joints.thumb[3], 0.026 * h));
    out.push(sphere(CHAIN_THUMB, MAT_SKIN, joints.thumb[2], 0.028 * h));
    let thumb_nail = joints.thumb[3].add(joints.thumb_tip_frame.fwd.scale(-0.014 * h));
    out.push(sphere(CHAIN_THUMB, MAT_NAIL, thumb_nail, 0.022 * h));

    out
}

/// `gather_primitives` never emits more than this many - checked by
/// `every_pose_stays_within_the_primitive_budget` since the uniform array
/// is a fixed size and silently dropping a primitive would be a worse bug
/// than a panic.
const MAX_PRIM: usize = 48;

// ------------------------------------------------------------------- gpu

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    resolution: [f32; 2],
    prim_count: f32,
    _pad0: f32,
    eye: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    fwd: [f32; 4],
    /// focal length, hue (degrees), chroma, within-finger blend radius.
    focal_hue_chroma_kfinger: [f32; 4],
    /// bulb position (xyz), light attenuation coefficient.
    light_attk: [f32; 4],
    /// box half-width, depth, ceiling height, floor height.
    box_dims: [f32; 4],
    /// contact shadow: centre x, centre z, strength, radius^2.
    shadow: [f32; 4],
    /// ambient, key (point-light) strength, subsurface/translucency, rim.
    light_strength: [f32; 4],
    /// AO strength, nail highlight boost, unused, floor dark cutoff.
    ao_nail_kpalm_darkcut: [f32; 4],
    /// floor's max lightness once lit, palm's own self-blend radius, unused.
    maxl_kpalmself: [f32; 4],
    prims: [Prim; MAX_PRIM],
}

struct Live {
    gpu: &'static Gpu,
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    target: Offscreen,
}

/// Complete renders averaged per output frame, spread across its own
/// interval - the motion blur (the autumn set's "temporal supersampling
/// across the frame interval", the same technique `ghosts` uses).
const BLUR_SAMPLES: usize = 4;
/// Samples per axis per LED inside each of those renders.
const SUPERSAMPLES: u32 = 6;

fn open_gpu() -> Option<Live> {
    let gpu = match Gpu::shared() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("screeny-art: thing: {e}; rendering black");
            return None;
        }
    };
    let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("thing"),
        source: wgpu::ShaderSource::Wgsl(format!("{COMMON_WGSL}\n{}", include_str!("thing.wgsl")).into()),
    });
    let pipeline = gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("thing"),
        layout: None,
        vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(COLOR_FORMAT.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    let uniforms = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("thing uniforms"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
    });
    Some(Live { gpu, pipeline, uniforms, bind_group, target: Offscreen::new(gpu, SUPERSAMPLES) })
}

// ------------------------------------------------------------- lighting

/// Cool-warm bulb tone shared by the hand and the floor's own pool of
/// light, so the two read as lit by the same source rather than two
/// independently coloured things.
const HUE: f32 = 56.0;
const BASE_CHROMA: f32 = 0.05;

const AMBIENT: f32 = 0.28;
const KEY_STRENGTH: f32 = 1.45;
/// "A hint of subsurface warmth at the edges" (the card).
const BACK_STRENGTH: f32 = 0.16;
const RIM_STRENGTH: f32 = 0.22;
/// "Ambient occlusion between fingers" (the card).
const AO_STRENGTH: f32 = 2.6;
const NAIL_BOOST: f32 = 0.10;

const SHADOW_STRENGTH: f32 = 0.55;
const SHADOW_RADIUS2: f32 = 0.0028;

// Card 326 review's own words apply here too - "the box is not the light in
// the frame, the subject is": pulled the floor's own visible pool in tight
// (a high `DARK_CUT`, most of the floor snaps true black) and its peak
// lightness down, so the hand reads as the brightest, most detailed thing
// in the picture rather than competing with a wide floor gradient.
const DARK_CUT: f32 = 0.32;
const MAX_L: f32 = 0.5;

/// Blend radii, world metres at `HAND_H`'s own scale: small within one
/// finger (round the joints without eating the taper), a little fleshier
/// for the palm's own fan of capsules. Where a finger meets the palm, and
/// where two fingers are near each other, `scene_sdf` unions with a plain
/// `min`, not a third blend radius here - see its own doc for the real bug
/// (fingers reading as one mitten) a smooth join at that seam caused.
const K_FINGER: f32 = 0.006;
const K_PALM_SELF: f32 = 0.013;

const STEPS: usize = 26;
const DARK_L: f32 = 0.02;
const LIGHT_L: f32 = 0.95;

impl Thing {
    fn render_gpu(&mut self, ctx: &Ctx) -> Frame {
        let light = ctx.get("light");
        let glow = ctx.get("glow");
        let color = ctx.get("color").clamp(0.0, 1.0);
        let chroma = BASE_CHROMA * color;

        // Motion blur interpolates between the choreographer's own two most
        // recent fixed steps (`choreo.rs`'s doc explains why that pair,
        // rather than a re-evaluation at an arbitrary time, is right here:
        // the player's contact pinning is real, once-a-step state). The
        // camera is not motion-blurred - its own drift is real-time and
        // negligible over one frame - so it is computed once, outside the
        // loop, exactly as card 333's CPU render did.
        let (prev, curr) = self.choreo.snapshot();
        let Some(curr) = curr else { return Frame::black() };
        let prev = prev.unwrap_or(curr);
        let cam = Camera::at(ctx.t, crate::frame::W);

        let mut sum: Option<Vec<crate::color::Rgb>> = None;
        for i in 0..BLUR_SAMPLES {
            let u = (i as f32 + 0.5) / BLUR_SAMPLES as f32;
            let posed = interp(prev, curr, u, ctx.t);
            let root = keep_in_box(posed.root_pos);
            let joints = posed.pose.solve(root, posed.root_frame, HAND_H);
            let prims = gather_primitives(&joints, HAND_H);
            debug_assert!(prims.len() <= MAX_PRIM, "{} primitives, budget is {MAX_PRIM}", prims.len());

            let Some(Some(live)) = self.live.as_mut() else { unreachable!("opened before this is called") };

            let mut prim_buf = [Prim { meta: [0.0; 4], a: [0.0; 4], b: [0.0; 4] }; MAX_PRIM];
            for (slot, p) in prims.iter().take(MAX_PRIM).enumerate() {
                prim_buf[slot] = *p;
            }
            let (w, h) = live.target.size();
            // `cam.focal` is calibrated in LED units (`Camera::at` builds it
            // from `crate::frame::W`, 64), but `fs_main` reads pixel
            // coordinates in the *supersampled* target's own device pixels
            // (`@builtin(position)`, `0..w`/`0..h` here) - scaling focal by
            // the same supersample factor keeps the two in the same units,
            // which is what actually fixes the field of view rather than
            // widening it by `SUPERSAMPLES`x (a ray straight through the
            // middle of the box, missing the hand almost everywhere, is
            // what this looked like before the fix was measured rather than
            // assumed correct from the ported formula alone).
            let focal = cam.focal * SUPERSAMPLES as f32;
            let uniforms = Uniforms {
                resolution: [w as f32, h as f32],
                prim_count: prims.len().min(MAX_PRIM) as f32,
                _pad0: 0.0,
                eye: [cam.eye.x, cam.eye.y, cam.eye.z, 0.0],
                right: [cam.right.x, cam.right.y, cam.right.z, 0.0],
                up: [cam.up.x, cam.up.y, cam.up.z, 0.0],
                fwd: [cam.fwd.x, cam.fwd.y, cam.fwd.z, 0.0],
                focal_hue_chroma_kfinger: [focal, HUE, chroma, K_FINGER],
                light_attk: [box_scene::LIGHT.x, box_scene::LIGHT.y, box_scene::LIGHT.z, box_scene::ATT_K],
                box_dims: [box_scene::HALF_W, box_scene::DEPTH, box_scene::CEIL_Y, box_scene::FLOOR_Y],
                shadow: [root.x, root.z, SHADOW_STRENGTH, SHADOW_RADIUS2],
                light_strength: [AMBIENT * light, KEY_STRENGTH * light, BACK_STRENGTH * glow, RIM_STRENGTH * glow],
                ao_nail_kpalm_darkcut: [AO_STRENGTH, NAIL_BOOST, 0.0, DARK_CUT],
                maxl_kpalmself: [MAX_L * light, K_PALM_SELF, 0.0, 0.0],
                prims: prim_buf,
            };
            live.gpu.queue.write_buffer(&live.uniforms, 0, bytemuck::bytes_of(&uniforms));

            let mut encoder = live.gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = live.target.pass(&mut encoder);
                pass.set_pipeline(&live.pipeline);
                pass.set_bind_group(0, &live.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            let frame = live.target.finish(live.gpu, encoder);
            let px = frame.to_linear();
            sum = Some(match sum {
                None => px,
                Some(acc) => acc.iter().zip(px.iter()).map(|(a, b)| a.add(*b)).collect(),
            });
        }

        let Some(sum) = sum else { return Frame::black() };
        let norm = 1.0 / BLUR_SAMPLES as f32;
        let px: Vec<crate::color::Rgb> = sum.iter().map(|c| c.scale(norm)).collect();
        Palette::ramps(&[HUE], STEPS, (DARK_L, LIGHT_L), chroma).map(&Frame::Linear(px), Dither::BlueNoise, 0.6)
    }
}

impl Patch for Thing {
    fn playing(&self) -> Option<Playing> {
        Some(self.choreo.playing())
    }

    fn act(&mut self, action: &str) {
        self.choreo.act(action);
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.advance(ctx);
        if self.live.is_none() {
            self.live = Some(open_gpu());
        }
        if matches!(self.live, Some(None)) {
            return Frame::black();
        }
        self.render_gpu(ctx)
    }
}

#[cfg(test)]
mod tests;
