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

pub(crate) mod act;
pub(crate) mod cloth;

use crate::dither::Dither;
use crate::frame::{Frame, H, N, W};
use crate::gpu::mat::{self, Mat4};
use crate::gpu::{Gpu, Offscreen, COLOR_FORMAT, COMMON_WGSL, DEPTH_FORMAT};
use crate::palette::Palette;
use crate::patch::{param, Ctx, ParamSpec, Patch, PatchDef, Playing};
use act::{Director, Pose};
use cloth::{Cloth, HeadTarget, V3, VERTS};
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
    param("size", "How big they are (LEDs tall)", 8.0, 30.0, 0.5, 22.0),
    param("sway", "How loose/floppy the cloth is", 0.0, 1.0, 0.01, 0.45),
    param("glow", "Soft glow", 0.0, 1.0, 0.01, 0.28),
    param("color", "Colour (0 = grayscale)", 0.0, 1.0, 0.01, 0.32),
    param("hue", "Tint, when colour is on (0 = red, the cool default is ~205)", 0.0, 360.0, 1.0, 205.0),
    param("eyes", "Eye size", 0.5, 2.0, 0.05, 1.0),
    param("eye_light", "Eye luminance (0 = true-black holes, 1 = a soft glow)", 0.0, 1.0, 0.01, 0.0),
    param("eye_hue", "Eye tint, once eye_light > 0", 0.0, 360.0, 1.0, 205.0),
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

/// A head's rigid placement in world space, from its pose.
fn head_of(pose: &Pose) -> HeadTarget {
    HeadTarget {
        pos: V3::new(pose.cx - W as f32 / 2.0, H as f32 / 2.0 - pose.cy, -(REF_DISTANCE + pose.depth)),
        yaw: pose.yaw,
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
    eyes: [[f32; 4]; MAX_GHOSTS],
    light: [f32; 4],
    params: [f32; 4],
    params2: [f32; 4],
    /// Per-slot body opacity (`Shape::alpha`) - "where two overlap, or it
    /// crosses something, you can tell" (card 315, kept in the 3D pass).
    alpha: [f32; 4],
    /// `[er_u, er_v, edge, unused]` - see `ghosts.wgsl`'s `Scene.eye_shape`.
    eye_shape: [f32; 4],
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
// The ghost is meant to be the light in the frame (card 326 review: "a soft,
// luminous pale sheet ... brightest on the dome, folds as gentle shade, not
// a mostly-mid-grey body") - raised twice now: 0.09 first, then 0.24 still
// read as a mid-grey body with one bright corner rather than a lit sheet.
const AMBIENT: f32 = 0.4;
const KEY_STRENGTH: f32 = 0.62;
/// "A little translucency ... a faint glow ... if it reads" (card 326): a
/// soft light-through-fabric term on the shadow side, and a view-dependent
/// rim, both riding the same `glow` param as the background bloom below.
const BACK_STRENGTH: f32 = 0.16;
const RIM_STRENGTH: f32 = 0.22;
/// "Soft shadows in the folds": how hard the curvature-based AO term bites.
const AO_STRENGTH: f32 = 1.1;

const BASE_CHROMA: f32 = 0.1;
const DARK_L: f32 = 0.02;
const LIGHT_L: f32 = 0.95;
const STEPS: usize = 30;

// Eye level sits on the dome's own round part (a head ring short of the
// neck), not the neck or the skirt. Card 326 review: "~2x2-2x3 LEDs each
// with at least 2 lit LEDs between them" at default size - tuned against the
// actual 64x32 output (`Frame::pixel`, not the supersampled buffer) rather
// than assumed from the UV numbers alone. `EYE_R_U`/`EYE_R_V` are separate
// because a UV unit is not the same physical size in both directions (`u`
// wraps the whole head, `v` only runs crown to hem) - a single radius drew a
// hole about one LED tall and three wide, not the roughly round hole wanted.
const EYE_V: f32 = 0.2;
const EYE_DX: f32 = 0.095;
const EYE_R_U: f32 = 0.046;
const EYE_R_V: f32 = 0.082;
const EYE_EDGE: f32 = 0.14;
const GAZE_UV: f32 = 0.045;

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

/// One ghost's mesh and everything the renderer needs at one instant.
struct Drawn {
    key: u64,
    mesh: Vec<cloth::RenderVertex>,
    gaze: (f32, f32),
    alpha: f32,
    z: f32,
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
                Drawn { key: pose.key, mesh: cloth.render_vertices(), gaze: pose.gaze, alpha: pose.shape.alpha, z: head.pos.z }
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

        for &sub_t in &times {
            let drawn = self.frame_at(sub_t, sway);
            for d in &drawn {
                seen.insert(d.key);
            }

            let hue = ctx.get("hue");
            let chroma = BASE_CHROMA * ctx.get("color").clamp(0.0, 1.0);
            let glow = ctx.get("glow").clamp(0.0, 1.0);
            let eye_scale = ctx.get("eyes").clamp(0.5, 2.0);
            let eye_light = ctx.get("eye_light").clamp(0.0, 1.0);
            let eye_hue = ctx.get("eye_hue");

            let Some(Some(live)) = self.live.as_mut() else { unreachable!("opened before this is called") };

            let mut verts = vec![
                Vertex { pos: [0.0; 3], normal: [0.0, 1.0, 0.0], uv: [0.0; 2], fold: 0.0, slot: 0.0 };
                MAX_GHOSTS * VERTS
            ];
            let mut eyes = [[0.5_f32, EYE_V, 0.5, EYE_V]; MAX_GHOSTS];
            let mut alpha = [1.0_f32; MAX_GHOSTS];
            for (slot, d) in drawn.iter().enumerate() {
                // The vertical component gets its own factor, not the same
                // number as the horizontal one: `u` and `v` are different
                // physical scales (see `EYE_R_U`/`EYE_R_V`), so an equal UV
                // shift would move the eyes further, in LEDs, up/down than
                // side to side.
                let shift = (d.gaze.0.clamp(-1.0, 1.0) * GAZE_UV, d.gaze.1.clamp(-1.0, 1.0) * GAZE_UV * (EYE_R_V / EYE_R_U) * 0.6);
                eyes[slot] = [0.5 - EYE_DX - shift.0, EYE_V - shift.1, 0.5 + EYE_DX - shift.0, EYE_V - shift.1];
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
                eyes,
                light: [KEY_LIGHT[0], KEY_LIGHT[1], KEY_LIGHT[2], 0.0],
                params: [hue, chroma, eye_light, AO_STRENGTH],
                params2: [AMBIENT, KEY_STRENGTH, BACK_STRENGTH, RIM_STRENGTH * glow],
                alpha,
                eye_shape: [EYE_R_U * eye_scale, EYE_R_V * eye_scale, EYE_EDGE, eye_hue],
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
        }

        self.clothes.retain(|k, _| seen.contains(k));

        let norm = 1.0 / times.len() as f32;
        let px: Vec<crate::color::Rgb> = sum.unwrap_or_else(|| vec![crate::color::Rgb::BLACK; N]).iter().map(|c| c.scale(norm)).collect();
        let bloomed = bloom(&px, ctx.get("glow").clamp(0.0, 1.0));

        let hue = ctx.get("hue");
        let chroma = BASE_CHROMA * ctx.get("color").clamp(0.0, 1.0);
        Palette::ramps(&[hue], STEPS, (DARK_L, LIGHT_L), chroma).map(&Frame::Linear(bloomed), Dither::BlueNoise, 0.6)
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
    const RADIUS: i32 = 2;
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


    /// The same seed and the same moment draw the same frame, exactly -
    /// `screeny-art snapshot --seed N --at S` is a promise, even though the
    /// picture now goes through a cloth sim and a GPU pipeline to get there.
    #[test]
    fn a_seed_and_a_moment_are_deterministic() {
        for at in [1.5, 4.0, 6.5] {
            assert_eq!(colours(&frame_at(7, at, &[])), colours(&frame_at(7, at, &[])), "at {at}s");
        }
    }

    /// At `color` 0 the frame is grayscale: every pixel's three channels are
    /// equal (the card's grayscale rule, checked rather than assumed) - true
    /// whether a GPU adapter is available (a real lit render) or not (the
    /// black fallback, trivially grey).
    #[test]
    fn color_zero_is_grayscale() {
        for at in [1.0, 3.0, 5.0] {
            let frame = frame_at(3, at, &[("color", 0.0)]);
            for c in colours(&frame) {
                assert!((c[0] - c[1]).abs() < 1e-3 && (c[1] - c[2]).abs() < 1e-3, "at {at}s: {c:?} is not grey");
            }
        }
    }

    /// Never a full-white frame, and the average level stays low - this runs
    /// off laptop USB (the common brightness rule).
    #[test]
    fn never_bright_and_never_a_lot_of_it() {
        for at in [2.0, 5.0, 8.0] {
            let frame = frame_at(1, at, &[("ghosts", 4.0), ("glow", 1.0)]);
            let px = colours(&frame);
            let apl = px.iter().map(|c| (c[0] + c[1] + c[2]) / 3.0).sum::<f32>() / px.len() as f32;
            assert!(apl < 0.16, "at {at}s: average picture level {apl}");
            assert!(px.iter().all(|c| c[0] < 0.97 && c[1] < 0.97 && c[2] < 0.97), "at {at}s: something is at full white");
        }
    }

    /// The owner's own words on `eye_light`: "never full white" - checked at
    /// its own maximum, on top of everything else that pushes brightness up
    /// (`glow`, `ghosts` at its cap, and the biggest `eyes` size).
    #[test]
    fn glowing_eyes_are_never_full_white() {
        for at in [2.0, 5.0, 8.0] {
            let frame = frame_at(1, at, &[("ghosts", 4.0), ("glow", 1.0), ("eye_light", 1.0), ("eyes", 2.0)]);
            let px = colours(&frame);
            assert!(px.iter().all(|c| c[0] < 0.97 && c[1] < 0.97 && c[2] < 0.97), "at {at}s: something is at full white");
        }
    }

    /// Every frame is a small exact palette, whatever the picture: the
    /// GUARANTEED_PALETTE promise, unchanged by the rework.
    #[test]
    fn every_frame_is_a_small_exact_palette() {
        let frame = frame_at(5, 4.0, &[("ghosts", 4.0)]);
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
