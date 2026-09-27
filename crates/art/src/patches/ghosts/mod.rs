//! `ghosts`: classic sheet ghosts, each one real cloth draped over an
//! invisible head, simulated and lit in 3D.
//!
//! Card 315 drew the first pass as a flat, anti-aliased 2D silhouette - "a
//! rounded dome head... a wavy, scalloped hem". The owner, having seen it on
//! the panel (2026-09-26): "the ghosts should be 3d rendered, and perhaps
//! with fabric so they can sway like a sheet when they move." This is that
//! rework, same id, same choreography ([`act`]'s drift/bounce/peek/swoop/
//! cross/chase, recalibrated calmer - card 326: "usually one ghost... long
//! empty moments... bounce and chase become rare accents"), new picture.
//!
//! **The cloth** is [`cloth::Cloth`]: a poncho-shaped mesh, glued to the head
//! at its crown and collar, hanging free below that in a simulated skirt -
//! see that module for why it wraps all the way round rather than facing the
//! camera flat. [`act::Pose`] hands this module a head position (`cx, cy`
//! in panel coordinates, `depth`, `yaw`) at any instant; the cloth's own
//! physics is what turns that motion into lag and sway, not anything drawn
//! here.
//!
//! **The render** is a real mesh pipeline through `wgpu` (the house pattern
//! is `knot`'s: vertex/index buffers, a depth buffer, a supersampled
//! [`crate::gpu::Offscreen`]), lit by one hue "ray" - `ghosts.wgsl`'s
//! `oklch(l, chroma, hue)` - so `color = 0` is exactly grayscale by
//! construction, the same guarantee `vesta` and the first pass built by
//! hand. Motion blur is literal temporal supersampling: several complete
//! renders spread across the frame's own interval, averaged in linear
//! light, per the owner's later direction that compute here is not a
//! constraint and the picture should not cut corners.
//!
//! **The background is true black** (the owner, 2026-09-26: "we'll prefer
//! foreground animations against a black backdrop"): no ground line, no
//! stars - the ghost, and its own glow, is the only light in the frame.
//!
//! **The face** ([`face`]) is stamped separately, after the cloth render:
//! card 336 cut it into the mesh's own curved, supersampled, motion-blurred
//! UV surface and its own log named why that cannot make a solid dark block
//! at 64x32; card 338 projects the head's face centre into panel space
//! instead and paints hand-drawn pixel-art glyphs straight onto the frame.

pub(crate) mod act;
pub(crate) mod cloth;
mod face;

use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::gpu::mat::{self, Mat4};
use crate::gpu::{Gpu, Offscreen, COLOR_FORMAT, COMMON_WGSL, DEPTH_FORMAT};
use crate::palette::Palette;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use act::{Director, Pose};
use cloth::{BodyPose, Cloth, V3, VERTS};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

pub const DEF: PatchDef = PatchDef {
    id: "ghosts",
    name: "Ghosts",
    blurb: "GPU, cloth sheets draped over invisible heads, lit and swaying in 3D: drifting, peeking, and rarer bounces, swoops, crossings and chases.",
    params: PARAMS,
    make,
    // The schedule, every shape's proportions, and its turn/depth breathing
    // are all drawn from the seed: a new one is a different night of ghosts
    // (card 151).
    seeded: true,
};

const PARAMS: &[ParamSpec] = &[
    param("ghosts", "How many at once", 1.0, 4.0, 1.0, 2.0),
    param("pace", "How fast they move (lower is slower, dreamier)", 0.3, 2.2, 0.05, 0.85),
    param("bounce", "Bouncy vs floaty (bounce and chase stay rare accents either way)", 0.0, 1.0, 0.01, 0.25),
    param("size", "How big they are (LEDs tall, crown to hem)", 16.0, 32.0, 0.5, 28.0),
    param("sway", "How loose/floppy the cloth is", 0.0, 1.0, 0.01, 0.45),
    param("glow", "Soft glow", 0.0, 1.0, 0.01, 0.08),
    param("color", "Colour (0 = grayscale)", 0.0, 1.0, 0.01, 0.32),
    param("hue", "Tint, when colour is on (0 = red, the cool default is ~205)", 0.0, 360.0, 1.0, 205.0),
    param("eyes", "Eye size", 0.5, 2.0, 0.05, 1.0),
    param("eye_light", "Eye luminance (0 = true-black holes, 1 = a soft glow)", 0.0, 1.0, 0.01, 0.0),
    param("eye_hue", "Eye tint, once eye_light > 0", 0.0, 360.0, 1.0, 205.0),
    param("mouth", "Mouth size (0 = none)", 0.0, 2.0, 0.05, 1.0),
];

/// Same cap the `ghosts` param allows, and the size every per-frame array
/// below is fixed at.
const MAX_GHOSTS: usize = 4;
/// Complete renders averaged per output frame, spread across its own
/// interval, in linear light - the motion blur (card 326's later
/// direction: "motion blur (temporal supersampling across the frame
/// interval)").
const BLUR_SAMPLES: usize = 4;
/// Samples per axis per LED inside each of those renders.
const SUPERSAMPLES: u32 = 6;

/// World units from the camera to the reference plane (`depth == 0`); world
/// `x`/`y` are panel LED units directly, so a head at `depth == 0` sits
/// exactly the size a flat silhouette of the same `size` used to be, and the
/// field of view is chosen so that plane exactly fills the frame.
const REF_DISTANCE: f32 = 40.0;

fn fov_y() -> f32 {
    2.0 * (H as f32 / 2.0 / REF_DISTANCE).atan()
}

/// A body's rigid placement in world space, from its pose.
fn head_of(pose: &Pose) -> BodyPose {
    BodyPose {
        pos: V3::new(pose.cx - W as f32 / 2.0, H as f32 / 2.0 - pose.cy, -(REF_DISTANCE + pose.depth)),
        yaw: pose.yaw,
        arms: pose.arms,
    }
}

// ---------------------------------------------------------------- the mesh

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    fold: f32,
    slot: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Scene {
    view_proj: Mat4,
    light: [f32; 4],
    /// `[hue, chroma, ao_strength, unused]` - the face (eyes, mouth,
    /// `eye_light`/`eye_hue`) is no longer this shader's concern; see
    /// [`face`].
    params: [f32; 4],
    params2: [f32; 4],
    /// Per-slot body opacity (`Shape::alpha`) - "where two overlap, or it
    /// crosses something, you can tell" (card 315, kept in the 3D pass).
    alpha: [f32; 4],
}

/// The mesh's triangles, flattened for the GPU index buffer - one
/// implementation ([`cloth::triangles`]) shared with the physics body's own
/// collision surface, rather than two triangulations that could drift apart.
fn build_indices() -> Vec<u32> {
    cloth::triangles().into_iter().flatten().collect()
}

// ---------------------------------------------------------------- lighting

/// Cool moonlight from high and slightly behind-left, plus a dim ambient - a
/// single fixed direction is enough at this scale and keeps every ghost lit
/// the same way regardless of where it has turned to.
const KEY_LIGHT: [f32; 3] = [-0.35, 0.82, 0.45];
// The ghost is meant to be the light in the frame, and the reference photo
// is a near-white, fairly uniform sheet - not a mid-grey body. Raised again,
// a third time (0.09, then 0.24, then 0.4): the owner's own side-by-side
// against the reference called the sheet "dithered mid-grey" against the
// reference's near-white. `KEY_STRENGTH` raised to match - together these
// put most of the sheet close to `LIGHT_L` (below), the brightest thing
// drawn, capped well short of full white by the palette's own `LIGHT_L`.
const AMBIENT: f32 = 0.58;
const KEY_STRENGTH: f32 = 0.85;
/// "A little translucency ... a faint glow ... if it reads" (card 326): a
/// soft light-through-fabric term on the shadow side, and a view-dependent
/// rim - both kept subtle now that the sheet itself is meant to be bright,
/// so neither reads as its own separate glow on top of an already-light
/// surface.
const BACK_STRENGTH: f32 = 0.08;
const RIM_STRENGTH: f32 = 0.12;
/// "Soft shadows in the folds": how hard the curvature-based AO term bites.
/// Lowered from `1.1`: the owner's review asked for "subtle, linear
/// darkening" in the folds, not the heavy shading a near-white reference
/// sheet does not show.
const AO_STRENGTH: f32 = 0.45;

const BASE_CHROMA: f32 = 0.1;
const DARK_L: f32 = 0.02;
const LIGHT_L: f32 = 0.95;
const STEPS: usize = 30;

// ---------------------------------------------------------------- the GPU

struct Live {
    gpu: &'static Gpu,
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    scene: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    target: Offscreen,
    view_proj: Mat4,
}

fn open_gpu() -> Option<Live> {
    let gpu = match Gpu::shared() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("screeny-art: ghosts: {e}; rendering black");
            return None;
        }
    };
    let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ghosts"),
        source: wgpu::ShaderSource::Wgsl(format!("{COMMON_WGSL}\n{}", include_str!("ghosts.wgsl")).into()),
    });
    let pipeline = gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ghosts"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32, 4 => Float32],
            })],
        },
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: COLOR_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    let indices = build_indices();
    let vertices = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ghosts vertices"),
        size: (MAX_GHOSTS * VERTS * std::mem::size_of::<Vertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let index_buf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("ghosts indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let scene = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ghosts scene"),
        size: std::mem::size_of::<Scene>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: scene.as_entire_binding() }],
    });
    let proj = mat::perspective(fov_y(), W as f32 / H as f32, 1.0, 240.0);
    Some(Live {
        gpu,
        pipeline,
        vertices,
        indices: index_buf,
        index_count: indices.len() as u32,
        scene,
        bind_group,
        target: Offscreen::new(gpu, SUPERSAMPLES),
        view_proj: proj,
    })
}

// ---------------------------------------------------------------- the patch

struct Ghosts {
    director: Director,
    live: Option<Option<Live>>,
    clothes: HashMap<u64, Cloth>,
}

/// One ghost's mesh and everything the renderer needs at one instant -
/// `head_pos`/`yaw`/`head_r` are what [`face`] projects the face from, kept
/// alongside the mesh rather than recomputed, since `head_of` already built
/// them for the mesh itself.
struct Drawn {
    key: u64,
    mesh: Vec<cloth::RenderVertex>,
    gaze: (f32, f32),
    alpha: f32,
    z: f32,
    head_pos: V3,
    yaw: f32,
    head_r: f32,
}

fn make(seed: u64) -> Box<dyn Patch> {
    Box::new(Ghosts { director: Director::new(seed), live: None, clothes: HashMap::new() })
}

/// The sub-instants a frame's own motion blur is spread across: `samples`
/// points inside `(t - dt, t]`, ending exactly on `t` so a paused patch
/// (`dt == 0`) still draws the one instant that matters.
fn blur_times(t: f64, dt: f64, samples: usize) -> Vec<f64> {
    if dt <= 1e-9 {
        return vec![t];
    }
    (0..samples).map(|k| t - dt + dt * (k + 1) as f64 / samples as f64).collect()
}

impl Ghosts {
    /// Every ghost's mesh at `sub_t`, sorted farthest-first so translucent
    /// draw order composites correctly, and the set of keys touched (so the
    /// caller knows which cloths are still wanted this frame).
    fn frame_at(&mut self, sub_t: f64, sway: f32) -> Vec<Drawn> {
        let poses = self.director.poses_at(sub_t);
        let director = &self.director;
        let mut drawn: Vec<Drawn> = poses
            .iter()
            .map(|pose| {
                let head = head_of(pose);
                let cloth = self.clothes.entry(pose.key).or_insert_with(|| Cloth::spawn(pose.shape, head, sway));
                let key = pose.key;
                cloth.advance(sub_t, sway, |tt| {
                    director.poses_at(tt).into_iter().find(|p| p.key == key).map(|p| head_of(&p)).unwrap_or(head)
                });
                Drawn {
                    key: pose.key,
                    mesh: cloth.render_vertices(),
                    gaze: pose.gaze,
                    alpha: pose.shape.alpha,
                    z: head.pos.z,
                    head_pos: head.pos,
                    yaw: head.yaw,
                    head_r: pose.shape.head_r,
                }
            })
            .collect();
        drawn.sort_by(|a, b| a.z.total_cmp(&b.z));
        drawn.truncate(MAX_GHOSTS);
        drawn
    }

    fn render_gpu(&mut self, ctx: &Ctx) -> Frame {
        let sway = ctx.get("sway");
        let times = blur_times(ctx.t, ctx.dt.max(0.0), BLUR_SAMPLES);
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut sum: Option<Vec<crate::color::Rgb>> = None;
        // Every ghost drawn on the last sub-instant - `blur_times` always
        // ends its list exactly on `ctx.t`, never a blurred sample, so this
        // is the one, crisp pose the face gets stamped from (see below).
        let mut last_drawn: Vec<Drawn> = Vec::new();

        let hue = ctx.get("hue");
        let chroma = BASE_CHROMA * ctx.get("color").clamp(0.0, 1.0);

        for &sub_t in &times {
            let drawn = self.frame_at(sub_t, sway);
            for d in &drawn {
                seen.insert(d.key);
            }

            let glow = ctx.get("glow").clamp(0.0, 1.0);

            let Some(Some(live)) = self.live.as_mut() else { unreachable!("opened before this is called") };

            let mut verts = vec![
                Vertex { pos: [0.0; 3], normal: [0.0, 1.0, 0.0], uv: [0.0; 2], fold: 0.0, slot: 0.0 };
                MAX_GHOSTS * VERTS
            ];
            let mut alpha = [1.0_f32; MAX_GHOSTS];
            for (slot, d) in drawn.iter().enumerate() {
                alpha[slot] = d.alpha;
                for (i, v) in d.mesh.iter().enumerate() {
                    verts[slot * VERTS + i] = Vertex {
                        pos: [v.pos.x, v.pos.y, v.pos.z],
                        normal: [v.normal.x, v.normal.y, v.normal.z],
                        uv: v.uv,
                        fold: v.fold,
                        slot: slot as f32,
                    };
                }
            }

            let scene = Scene {
                view_proj: live.view_proj,
                light: [KEY_LIGHT[0], KEY_LIGHT[1], KEY_LIGHT[2], 0.0],
                params: [hue, chroma, AO_STRENGTH, 0.0],
                params2: [AMBIENT, KEY_STRENGTH, BACK_STRENGTH, RIM_STRENGTH * glow],
                alpha,
            };
            live.gpu.queue.write_buffer(&live.vertices, 0, bytemuck::cast_slice(&verts));
            live.gpu.queue.write_buffer(&live.scene, 0, bytemuck::bytes_of(&scene));

            let mut encoder = live.gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = live.target.pass(&mut encoder);
                pass.set_pipeline(&live.pipeline);
                pass.set_bind_group(0, &live.bind_group, &[]);
                pass.set_vertex_buffer(0, live.vertices.slice(..));
                pass.set_index_buffer(live.indices.slice(..), wgpu::IndexFormat::Uint32);
                for slot in 0..drawn.len() {
                    pass.draw_indexed(0..live.index_count, (slot * VERTS) as i32, 0..1);
                }
            }
            let frame = live.target.finish(live.gpu, encoder);
            let px = frame.to_linear();
            sum = Some(match sum {
                None => px,
                Some(acc) => acc.iter().zip(px.iter()).map(|(a, b)| a.add(*b)).collect(),
            });
            last_drawn = drawn;
        }

        self.clothes.retain(|k, _| seen.contains(k));

        let norm = 1.0 / times.len() as f32;
        let px: Vec<crate::color::Rgb> = sum.unwrap_or_else(|| vec![crate::color::Rgb::BLACK; N]).iter().map(|c| c.scale(norm)).collect();
        let mut px = bloom(&px, ctx.get("glow").clamp(0.0, 1.0));

        // The face: stamped in panel space, after the cloth render and its
        // own motion blur and bloom - card 338's whole point (see the module
        // doc and `face`'s own). `last_drawn` is the exact instant `ctx.t`,
        // not a blurred sub-sample: the face wants one crisp pose, not a
        // motion-blurred one.
        let eyes_scale = ctx.get("eyes").clamp(0.5, 2.0);
        let eye_light = ctx.get("eye_light").clamp(0.0, 1.0);
        let eye_hue = ctx.get("eye_hue");
        let mouth_scale = ctx.get("mouth").clamp(0.0, 2.0);
        let base = px.clone();
        let occluders: Vec<face::Occluder> =
            last_drawn.iter().map(|d| face::Occluder { key: d.key, head_pos: d.head_pos, head_r: d.head_r }).collect();
        for d in &last_drawn {
            let input = face::FaceInput {
                key: d.key,
                head_pos: d.head_pos,
                yaw: d.yaw,
                head_r: d.head_r,
                gaze: d.gaze,
                hue,
                chroma,
                eyes_scale,
                eye_light,
                eye_hue,
                mouth_scale,
            };
            face::stamp(&mut px, &base, &input, &occluders);
        }

        Palette::ramps(&[hue], STEPS, (DARK_L, LIGHT_L), chroma).map(&Frame::Linear(px), Dither::BlueNoise, 0.6)
    }
}

/// A soft halo into the dark background around anything bright - "a faint
/// glow ... if it reads" reaching past the mesh's own silhouette, which a
/// lit surface shader alone cannot do. A small separable box blur, added
/// back scaled by `amount`; at `amount == 0` this is a no-op copy.
fn bloom(px: &[crate::color::Rgb], amount: f32) -> Vec<crate::color::Rgb> {
    if amount <= 0.0 {
        return px.to_vec();
    }
    const RADIUS: i32 = 1;
    let blur_pass = |src: &[crate::color::Rgb], horiz: bool| -> Vec<crate::color::Rgb> {
        let mut out = vec![crate::color::Rgb::BLACK; N];
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                let mut acc = crate::color::Rgb::BLACK;
                let mut n = 0.0_f32;
                for d in -RADIUS..=RADIUS {
                    let (sx, sy) = if horiz { (x + d, y) } else { (x, y + d) };
                    if (0..W as i32).contains(&sx) && (0..H as i32).contains(&sy) {
                        acc = acc.add(src[sy as usize * W + sx as usize]);
                        n += 1.0;
                    }
                }
                out[y as usize * W + x as usize] = acc.scale(1.0 / n.max(1.0));
            }
        }
        out
    };
    let blurred = blur_pass(&blur_pass(px, true), false);
    px.iter().zip(blurred.iter()).map(|(a, b)| a.add(b.scale(amount * 0.55))).collect()
}

impl Patch for Ghosts {
    fn playing(&self) -> Option<Playing> {
        let act = self.director.last()?;
        Some(Playing { title: "Ghosts".to_string(), detail: act.name.clone(), actions: Vec::new(), notes: Vec::new() })
    }

    fn render(&mut self, ctx: &Ctx) -> Frame {
        self.director.advance(ctx.t, ctx);
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
mod tests {
    use super::*;
    use crate::patch::Params;

    /// Render the patch itself (no pipeline, matching `vesta`'s, `flock`'s
    /// and the first pass's own tests): stepped at the snapshot tool's
    /// 30 fps from engine time zero up to `at`. Kept short - the GPU path
    /// does several real renders a frame, and these tests care about the
    /// picture's properties, not a long run (that is `act`'s own tests,
    /// which never touch the GPU).
    fn frame_at(seed: u64, at: f64, set: &[(&str, f32)]) -> Frame {
        let mut p = Params::defaults(DEF.params);
        for (k, v) in set {
            assert!(p.set(DEF.params, k, *v), "no parameter `{k}`");
        }
        let mut patch = (DEF.make)(seed);
        let dt = 1.0 / crate::snapshot::FPS;
        let steps = (at / dt).round() as usize;
        let mut frame = Frame::black();
        for i in 0..=steps {
            let t = i as f64 * dt;
            frame = patch.render(&Ctx { t, dt, now: t, params: &p });
        }
        frame
    }

    fn colours(frame: &Frame) -> Vec<[f32; 3]> {
        (0..crate::frame::N).map(|i| { let c = frame.pixel(i); [c.r, c.g, c.b] }).collect()
    }

    /// A pixel this dark, sitting right next to genuinely lit cloth, is a
    /// stamped feature (true black, `oklch(0.02, ..)`, next to bright fabric,
    /// no blend between them) - not open background, which borders more
    /// background, and not the sheet's own antialiased silhouette edge either
    /// (that fades over a pixel or two of motion blur, it does not jump
    /// straight from bright to near-zero).
    fn has_a_dark_pixel_embedded_in_bright_cloth(px: &[[f32; 3]]) -> bool {
        let luma = |c: &[f32; 3]| (c[0] + c[1] + c[2]) / 3.0;
        for y in 0..H {
            for x in 0..W {
                if luma(&px[y * W + x]) > 0.05 {
                    continue;
                }
                let neighbours = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)];
                if neighbours.iter().any(|&(nx, ny)| nx < W && ny < H && luma(&px[ny * W + nx]) > 0.5) {
                    return true;
                }
            }
        }
        false
    }

    /// The face really does cut solid dark blocks into the picture, not just
    /// a subtle darker patch - checked directly against the actual 64x32
    /// output (`Frame::pixel`, not the supersampled buffer), the same
    /// discipline 326's and 336's own logs name as the only reliable one.
    /// Unlike the box `eyes_cut_real_dark_holes_in_the_head` (336's own test,
    /// now obsolete: the face moved off the mesh's UV and onto the panel
    /// grid, so it no longer sits at a fixed, hand-found box of columns) this
    /// searches the whole frame for a dark pixel actually embedded in bright
    /// cloth, which is what "solid dark blobs against a bright sheet" means
    /// and stays true wherever this card's own geometry constants end up
    /// placing the face - a plain "some pixel is dark somewhere" would pass
    /// even with the face disabled entirely (the true-black background makes
    /// sure of that), so the check has to be more specific than that. Seed 5
    /// at 1s was found by scanning many (seed, moment) pairs for one where
    /// the head is already on-panel and facing the camera this early - most
    /// of an entrance is still off-panel or turned away (found by rendering
    /// and dumping, not assumed; see the card's Log) - cheap rather than the
    /// (10, 8.0) this card's own earlier draft used.
    #[test]
    fn the_face_is_a_solid_dark_block_against_a_bright_sheet() {
        let px = colours(&frame_at(5, 1.0, &[]));
        assert!(has_a_dark_pixel_embedded_in_bright_cloth(&px), "expected a real dark eye/mouth block in the lit sheet");
    }

    /// `eye_light` really does lighten the eyes end to end, through the whole
    /// render pipeline: every pixel that differs between `eye_light` 0 and 1
    /// is brighter at 1 - never the other way round, and never nothing at
    /// all (a param with no visible effect would be worse than useless).
    #[test]
    fn eye_light_lightens_the_eyes_end_to_end() {
        let luma = |c: &[f32; 3]| (c[0] + c[1] + c[2]) / 3.0;
        let dark = colours(&frame_at(5, 1.0, &[("eye_light", 0.0)]));
        let lit = colours(&frame_at(5, 1.0, &[("eye_light", 1.0)]));
        let mut changed = 0;
        for (a, b) in dark.iter().zip(lit.iter()) {
            if a != b {
                changed += 1;
                assert!(luma(b) > luma(a), "eye_light 1 did not lighten a pixel eye_light 0 left dark");
            }
        }
        assert!(changed > 0, "eye_light had no visible effect on the rendered frame");
    }

    /// `mouth` at 0 really does remove the mouth end to end: every pixel that
    /// differs between `mouth` 1 and 0 is brighter with the mouth off - the
    /// mouth only ever darkens, turning it off only ever brightens - and
    /// again, some pixel really does change.
    #[test]
    fn mouth_zero_removes_the_mouth_end_to_end() {
        let luma = |c: &[f32; 3]| (c[0] + c[1] + c[2]) / 3.0;
        let with_mouth = colours(&frame_at(5, 1.0, &[("mouth", 1.0)]));
        let without_mouth = colours(&frame_at(5, 1.0, &[("mouth", 0.0)]));
        let mut changed = 0;
        for (a, b) in with_mouth.iter().zip(without_mouth.iter()) {
            if a != b {
                changed += 1;
                assert!(luma(b) > luma(a), "removing the mouth made a pixel darker, not brighter");
            }
        }
        assert!(changed > 0, "the `mouth` param had no visible effect on the rendered frame");
    }

    /// The same seed and the same moment draw the same frame, exactly -
    /// `screeny-art snapshot --seed N --at S` is a promise, even though the
    /// picture now goes through a cloth sim and a GPU pipeline to get there.
    /// Kept to sub-2s moments - `at` itself is not what is under test here,
    /// and every simulated second is real physics in a debug binary (card
    /// 338's Log: keep the ghosts debug suite fast).
    #[test]
    fn a_seed_and_a_moment_are_deterministic() {
        for at in [0.3, 0.6] {
            assert_eq!(colours(&frame_at(7, at, &[])), colours(&frame_at(7, at, &[])), "at {at}s");
        }
    }

    /// At `color` 0 the frame is grayscale: every pixel's three channels are
    /// equal (the card's grayscale rule, checked rather than assumed) - true
    /// whether a GPU adapter is available (a real lit render) or not (the
    /// black fallback, trivially grey).
    #[test]
    fn color_zero_is_grayscale() {
        for at in [0.4, 0.9] {
            let frame = frame_at(3, at, &[("color", 0.0)]);
            for c in colours(&frame) {
                assert!((c[0] - c[1]).abs() < 1e-3 && (c[1] - c[2]).abs() < 1e-3, "at {at}s: {c:?} is not grey");
            }
        }
    }

    /// Never a full-white frame, and the average level stays low - this runs
    /// off laptop USB (the common brightness rule).
    /// Card 338's Log: every simulated ghost-second is real physics in a
    /// debug binary, and `ghosts` above 1 multiplies that by however many are
    /// actually on screen - `2.0` (still "more than one", the point of the
    /// cap this test cares about) rather than the full `4.0` a real run
    /// allows, and two moments rather than three.
    #[test]
    fn never_bright_and_never_a_lot_of_it() {
        for at in [0.5, 1.5] {
            let frame = frame_at(1, at, &[("ghosts", 2.0), ("glow", 1.0)]);
            let px = colours(&frame);
            let apl = px.iter().map(|c| (c[0] + c[1] + c[2]) / 3.0).sum::<f32>() / px.len() as f32;
            assert!(apl < 0.16, "at {at}s: average picture level {apl}");
            assert!(px.iter().all(|c| c[0] < 0.97 && c[1] < 0.97 && c[2] < 0.97), "at {at}s: something is at full white");
        }
    }

    /// The owner's own words on `eye_light`: "never full white" - checked at
    /// its own maximum, on top of everything else that pushes brightness up
    /// (`glow`, more than one ghost, and the biggest `eyes` size). See
    /// `never_bright_and_never_a_lot_of_it` on why `ghosts` is `2.0` here.
    #[test]
    fn glowing_eyes_are_never_full_white() {
        for at in [0.5, 1.5] {
            let frame = frame_at(1, at, &[("ghosts", 2.0), ("glow", 1.0), ("eye_light", 1.0), ("eyes", 2.0)]);
            let px = colours(&frame);
            assert!(px.iter().all(|c| c[0] < 0.97 && c[1] < 0.97 && c[2] < 0.97), "at {at}s: something is at full white");
        }
    }

    /// Every frame is a small exact palette, whatever the picture: the
    /// GUARANTEED_PALETTE promise, unchanged by the rework.
    #[test]
    fn every_frame_is_a_small_exact_palette() {
        let frame = frame_at(5, 1.0, &[("ghosts", 2.0)]);
        let Frame::Indexed { palette, .. } = frame else { panic!("ghosts must render indexed") };
        assert!(palette.len() <= crate::frame::GUARANTEED_PALETTE, "{} colours", palette.len());
    }

    /// Never more ghosts drawn than the `ghosts` param, through the whole
    /// pipeline (not just the director's own schedule, `act`'s test) - the
    /// mesh buffers are sized to `MAX_GHOSTS` and must never be asked to
    /// hold more.
    #[test]
    fn never_more_than_the_cap_reaches_the_renderer() {
        let mut p = Params::defaults(DEF.params);
        assert!(p.set(DEF.params, "ghosts", 4.0));
        let mut patch = Ghosts { director: Director::new(9), live: None, clothes: HashMap::new() };
        let dt = 1.0 / crate::snapshot::FPS;
        let mut t = 0.0;
        while t < 6.0 {
            let poses = {
                patch.director.advance(t, &Ctx { t, dt, now: t, params: &p });
                patch.director.poses_at(t)
            };
            assert!(poses.len() <= MAX_GHOSTS, "{} ghosts at t={t}", poses.len());
            t += dt;
        }
    }
}
