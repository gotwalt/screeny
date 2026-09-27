//! The GPU side: a lit, curved leaf mesh with a real depth buffer, rasterised
//! at heavy supersampling, then handed back to [`super`] for
//! [`crate::palette::Palette`] to bring inside the panel's exact colour
//! budget - the same shape `knot` (the house template for a mesh-based GPU
//! patch that must still go out exact) uses.
//!
//! **Built the way `knot` is, with one real difference.** `knot` is one
//! mesh, rotated by a `model` matrix the shader multiplies in; this patch may
//! be drawing anywhere from a couple of leaves to a couple of dozen (falling
//! ones, resting ones, and the fading motion-blur echoes of both), each with
//! its own position, its own quaternion orientation and its own build. Rather
//! than an instancing pipeline, every leaf's mesh is transformed to world
//! space **on the CPU**, once a frame, into one big vertex buffer - simpler
//! to get right than re-deriving quaternion rotation in WGSL, and at this
//! patch's own scale (a few dozen leaves at forty triangles each, at the
//! very most) not a cost worth avoiding. The owner's direction for this
//! card, once the picture needed it, was exactly that: "don't avoid the
//! GPU... compute cost is not a constraint either way. Spend it on image
//! quality."
//!
//! **Motion blur is temporal supersampling across the frame, not a
//! post-process blur.** [`super::LeavesPatch::advance`] already keeps the
//! last few fixed physics steps taken this frame (three, ordinarily, at 30
//! fps over a 1/90 s step); [`render`] draws each of them, oldest first,
//! alpha-blended and fading, with the newest solid on top. A slow linear
//! fall barely blurs at all this way, which is correct - real motion blur
//! would not show much of one either - but a leaf mid-tumble, whose
//! orientation can turn many degrees within one frame, gets a real
//! rotational smear from it.
//!
//! **Two pipelines, one pass.** Solid leaves (the newest step, and every
//! resting leaf) draw opaque, writing depth, so they correctly occlude one
//! another through the real z-buffer the card asks for. The fading echoes
//! behind them draw alpha-blended, depth-*tested* against that same buffer
//! (so a echo behind a solid leaf is properly hidden) but not depth-writing
//! (so overlapping echoes blend with each other instead of fighting).

use super::leaf::Leaf3D;
use super::{depth_fade, HueFamily, HUES};
use crate::frame::Frame;
use crate::gpu::mat::{self, Mat4};
use crate::gpu::{Gpu, Offscreen, COLOR_FORMAT, COMMON_WGSL, DEPTH_FORMAT};
use std::collections::VecDeque;
use wgpu::util::DeviceExt;

/// Supersamples per panel LED per axis. High on purpose - the owner, on this
/// card: "heavy supersampling... compute cost is not a constraint". At this
/// patch's triangle counts the cost is not measurable either way; the
/// picture is what it buys.
const SAMPLES: u32 = 8;

/// How big one "unit" leaf (half-length 1 in [`super::mesh`]'s local space)
/// is drawn, world metres, before [`super::leaf::Build::size`] scales it
/// further. A real leaf's full length, at `size = 1`, is twice this.
const HALF_LEN_M: f32 = 0.75;

/// The sun's elevation above the horizon, degrees - fixed; only its bearing
/// (`sun` param) is a taste the owner tunes. Low, the way an autumn sun (or a
/// moon) actually sits, per the card's own picture.
const SUN_ELEVATION_DEG: f32 = 24.0;

/// Camera clip planes, world metres, in front of the lens - see [`super`]'s
/// module doc for how a leaf's `(x, y, z)` becomes the GPU's own view space.
const CLIP_NEAR: f32 = 1.4;
const CLIP_FAR: f32 = 22.0;

/// Fading weights for the motion-blur echoes behind the newest (solid)
/// step, oldest first. One shorter than [`super::BLUR_TRAIL`] - the newest
/// step is always alpha 1 and is not in this list.
const BLUR_WEIGHTS: [f32; 2] = [0.32, 0.14];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GVertex {
    pos: [f32; 3],
    normal: [f32; 3],
    hue: f32,
    chroma: f32,
    top_l: f32,
    bot_l: f32,
    alpha: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Scene {
    view_proj: Mat4,
    eye: [f32; 4],
    light: [f32; 4],
}

/// One patch instance's own GPU resources. **Not a shared/global `OnceLock`**
/// like [`Gpu::shared`]'s device - the device is process-wide and stateless
/// between draws, but this owns a render target and a scene uniform buffer
/// that a frame is built into, and the studio may run more than one player
/// (and so more than one `LeavesPatch`) at once. `knot`'s own `Live` is kept
/// on the patch struct for exactly this reason; this does the same
/// (`super::LeavesPatch::gpu`), not a `static`.
pub(crate) struct Live {
    gpu: &'static Gpu,
    opaque_pipeline: wgpu::RenderPipeline,
    ghost_pipeline: wgpu::RenderPipeline,
    scene: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    target: Offscreen,
}

fn make_pipeline(
    gpu: &Gpu,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    blend: Option<wgpu::BlendState>,
    depth_write: bool,
    label: &str,
) -> wgpu::RenderPipeline {
    let attrs = wgpu::vertex_attr_array![
        0 => Float32x3, 1 => Float32x3, 2 => Float32, 3 => Float32, 4 => Float32, 5 => Float32, 6 => Float32
    ];
    let buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attrs,
    })];
    gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        // No culling: as `knot`'s and `flock`'s own doc comments both note
        // for their meshes, the depth buffer sorts occlusion out, and a
        // double-sided leaf must draw its underside when it is the side
        // facing the camera.
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(depth_write),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

pub(crate) fn open() -> Option<Live> {
    let gpu = match Gpu::shared() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("screeny-art: leaves: {e}; rendering black");
            return None;
        }
    };
    let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("leaves"),
        source: wgpu::ShaderSource::Wgsl(format!("{COMMON_WGSL}\n{}", include_str!("leaves.wgsl")).into()),
    });
    let bgl = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("leaves scene layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        }],
    });
    let pl = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("leaves pipeline layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let opaque_pipeline = make_pipeline(gpu, &module, &pl, None, true, "leaves opaque");
    let ghost_pipeline = make_pipeline(gpu, &module, &pl, Some(wgpu::BlendState::ALPHA_BLENDING), false, "leaves ghost");
    let scene = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("leaves scene"),
        size: std::mem::size_of::<Scene>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: scene.as_entire_binding() }],
    });
    Some(Live { gpu, opaque_pipeline, ghost_pipeline, scene, bind_group, target: Offscreen::new(gpu, SAMPLES) })
}

/// One leaf's mesh, transformed into the GPU's own view space (see
/// [`super`]'s module doc: `x` and `y` as they are, world `z` (a positive
/// depth, camera-forward) negated, which is what lets the vertex shader be a
/// single matrix multiply) and appended to `out`.
fn push_leaf(out: &mut Vec<GVertex>, leaf: &Leaf3D, families: &[HueFamily; HUES], color: f32, alpha: f32) {
    let fam = &families[leaf.build.hue as usize % HUES];
    let fade = depth_fade(leaf.pos.z);
    let (hue, chroma, top_l, bot_l) = (fam.hue, fam.chroma * color.max(0.0), fam.top_l * fade, fam.bot_l * fade);
    let scale = HALF_LEN_M * leaf.build.size;
    for v in super::mesh::build(leaf.build.shape, leaf.build.cup, leaf.build.curl) {
        let world = leaf.orient.rotate(v.pos.scale(scale)).add(leaf.pos);
        let normal = leaf.orient.rotate(v.normal);
        out.push(GVertex {
            pos: [world.x, world.y - super::EYE_Y, -world.z],
            normal: [normal.x, normal.y, -normal.z],
            hue,
            chroma,
            top_l,
            bot_l,
            alpha,
        });
    }
}

fn camera(sun_deg: f32) -> ([f32; 4], Mat4) {
    let proj = mat::perspective(super::VFOV_DEG.to_radians(), super::ASPECT, CLIP_NEAR, CLIP_FAR);
    let (elev_s, elev_c) = SUN_ELEVATION_DEG.to_radians().sin_cos();
    let (az_s, az_c) = sun_deg.to_radians().sin_cos();
    // Direction *to* the light, already in the GPU's z-negated space (see
    // `push_leaf`): a bearing around the scene at a fixed low elevation.
    let light = [elev_c * az_s, elev_s, -elev_c * az_c, 0.0];
    (light, proj)
}

impl Live {
    /// Render every falling leaf's recent physics history (newest last, per
    /// [`super::LeavesPatch::advance`]) and every resting leaf, and hand back
    /// the continuous, linear-light frame.
    #[must_use]
    pub(crate) fn render(
        &self,
        history: &[Vec<Leaf3D>],
        resting: &VecDeque<Leaf3D>,
        sun_deg: f32,
        color: f32,
        families: &[HueFamily; HUES],
        blur_trail: usize,
    ) -> Frame {
        let (light, proj) = camera(sun_deg);
        let scene = Scene { view_proj: proj, eye: [0.0, 0.0, 0.0, 1.0], light };
        self.gpu.queue.write_buffer(&self.scene, 0, bytemuck::bytes_of(&scene));

        let mut opaque = Vec::new();
        if let Some(newest) = history.last() {
            for leaf in newest {
                push_leaf(&mut opaque, leaf, families, color, 1.0);
            }
        }
        for leaf in resting {
            push_leaf(&mut opaque, leaf, families, color, 1.0);
        }

        // The echoes: up to `blur_trail - 1` steps immediately before the
        // newest one, nearest first (`age = 0` is one step back), each
        // faded by `BLUR_WEIGHTS`. `history`'s last entry is the newest/solid
        // step, never an echo.
        let mut ghosts = Vec::new();
        let n = history.len();
        let ghost_count = blur_trail.saturating_sub(1).min(n.saturating_sub(1));
        for age in 0..ghost_count {
            let idx = n - 2 - age;
            let weight = BLUR_WEIGHTS.get(age).copied().unwrap_or(0.05);
            for leaf in &history[idx] {
                push_leaf(&mut ghosts, leaf, families, color, weight);
            }
        }

        let buffer = |label, data: &[GVertex]| {
            self.gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(data),
                usage: wgpu::BufferUsages::VERTEX,
            })
        };

        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = self.target.pass(&mut encoder);
            pass.set_bind_group(0, &self.bind_group, &[]);
            // Opaque first, writing depth, so the echoes drawn after can be
            // depth-tested against real solid leaves and correctly hide
            // behind one that is actually nearer.
            if !opaque.is_empty() {
                let opaque_buf = buffer("leaves opaque", &opaque);
                pass.set_pipeline(&self.opaque_pipeline);
                pass.set_vertex_buffer(0, opaque_buf.slice(..));
                pass.draw(0..opaque.len() as u32, 0..1);
            }
            if !ghosts.is_empty() {
                let ghost_buf = buffer("leaves ghosts", &ghosts);
                pass.set_pipeline(&self.ghost_pipeline);
                pass.set_vertex_buffer(0, ghost_buf.slice(..));
                pass.draw(0..ghosts.len() as u32, 0..1);
            }
        }
        self.target.finish(self.gpu, encoder)
    }
}
