//! Full-frame shader patches: the quickest way to try a 3D idea. Write one WGSL
//! function, `fn shade(uv: vec2<f32>) -> vec3<f32>`, list its parameters, done.
//! See `fragment.wgsl` for what the shader is given and `patches/lattice.rs` for
//! a worked example.

use super::{Gpu, Offscreen, COLOR_FORMAT, COMMON_WGSL, DEPTH_FORMAT};
use crate::dither::Dither;
use crate::frame::Frame;
use crate::palette::Palette;
use crate::patch::{Ctx, ParamSpec, Patch};
use crate::rng::Rng;

const HARNESS_WGSL: &str = include_str!("fragment.wgsl");
/// `Uniforms.params` holds this many.
const MAX_PARAMS: usize = 16;
pub const MAX_EXTRA: usize = 16;
/// Palette entries a shader can paint with, and the length of the `palette`
/// array in `fragment.wgsl`. Its own number since card 102: a *frame* may have
/// up to `frame::MAX_PALETTE` colours, but a shader uniform is a fixed-size
/// array and this is the one place the two used to be the same constant by
/// accident. Raise it here and in the shader together.
pub const SCENE_PALETTE: usize = 32;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    resolution: [f32; 2],
    t: f32,
    dt: f32,
    seed: f32,
    _pad: [f32; 3],
    params: [f32; MAX_PARAMS],
    extra: [f32; MAX_EXTRA],
    palette: [[f32; 4]; SCENE_PALETTE],
}

/// What a patch's scene function works out on the CPU each frame.
pub struct Scene {
    /// Anything the shader needs that is not a parameter: light direction,
    /// phase of a cycle, camera state. Read in the shader as `X(i)`.
    pub extra: [f32; MAX_EXTRA],
    /// If set, the shader can paint with it (`PAL(i)`, `ramp()`), and the
    /// rendered frame is mapped onto it, so it is sent as an indexed frame.
    ///
    /// At most [`SCENE_PALETTE`] entries reach the shader - the rest are black
    /// in `PAL(i)` - even though a `Palette` may hold up to
    /// `frame::MAX_PALETTE`. Up to 32 the frame is exact whatever the indices
    /// look like; beyond that it is exact when the index image compresses,
    /// which for flat-shaded scene work it usually does. `Measured::exact`
    /// answers it per frame.
    pub palette: Option<Palette>,
    /// Strength of the ordered dither used by that mapping, 0..1.
    pub dither: f32,
}

/// `seed` is the same 0..1 value the shader sees as `u.seed`.
pub type SceneFn = fn(ctx: &Ctx, seed: f32) -> Scene;

struct Live {
    gpu: &'static Gpu,
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    target: Offscreen,
}

pub struct ShaderPatch {
    label: &'static str,
    source: &'static str,
    params: &'static [ParamSpec],
    /// Which parameter, if any, sets samples per axis.
    samples_param: Option<&'static str>,
    seed: f32,
    scene: Option<SceneFn>,
    /// `None` until first render; `Some(None)` if the GPU could not be opened.
    live: Option<Option<Live>>,
}

impl ShaderPatch {
    /// `source` must define `fn shade(uv: vec2<f32>) -> vec3<f32>`. Parameters
    /// reach it as `P(0)`, `P(1)`, ... in the order of `params`. If one of them
    /// is called `samples`, it sets the supersampling; otherwise 4 per axis.
    pub fn boxed(label: &'static str, source: &'static str, params: &'static [ParamSpec], seed: u64) -> Box<dyn Patch> {
        Self::build(label, source, params, seed, None)
    }

    /// As `boxed`, plus a function run on the CPU every frame to give the
    /// shader a palette and scene values. See `patches/overland.rs`.
    pub fn with_scene(
        label: &'static str,
        source: &'static str,
        params: &'static [ParamSpec],
        seed: u64,
        scene: SceneFn,
    ) -> Box<dyn Patch> {
        Self::build(label, source, params, seed, Some(scene))
    }

    fn build(
        label: &'static str,
        source: &'static str,
        params: &'static [ParamSpec],
        seed: u64,
        scene: Option<SceneFn>,
    ) -> Box<dyn Patch> {
        assert!(params.len() <= MAX_PARAMS, "{label}: at most {MAX_PARAMS} parameters");
        Box::new(ShaderPatch {
            label,
            source,
            params,
            samples_param: params.iter().map(|p| p.id).find(|id| *id == "samples"),
            seed: Rng::new(seed).f32(),
            scene,
            live: None,
        })
    }

    fn open(&self, samples: u32) -> Option<Live> {
        let gpu = match Gpu::shared() {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("screeny-art: {}: {e}; rendering black", self.label);
                return None;
            }
        };
        let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(self.label),
            source: wgpu::ShaderSource::Wgsl(format!("{COMMON_WGSL}\n{HARNESS_WGSL}\n{}", self.source).into()),
        });
        let pipeline = gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(self.label),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            // Unused by a full-frame shader, but it keeps every patch compatible
            // with the one kind of pass `Offscreen` hands out.
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
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        Some(Live { gpu, pipeline, uniforms, bind_group, target: Offscreen::new(gpu, samples) })
    }
}

impl Patch for ShaderPatch {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let samples = self.samples_param.map_or(4, |id| ctx.get(id) as u32);
        if self.live.is_none() {
            self.live = Some(self.open(samples));
        }
        let scene = self.scene.map(|f| f(ctx, self.seed));
        let Some(Some(live)) = self.live.as_mut() else { return Frame::black() };
        if live.target.samples() != samples.clamp(1, 16) {
            live.target = Offscreen::new(live.gpu, samples);
        }

        let (w, h) = live.target.size();
        let mut params = [0.0; MAX_PARAMS];
        for (slot, spec) in params.iter_mut().zip(self.params) {
            *slot = ctx.get(spec.id);
        }
        let uniforms = Uniforms {
            resolution: [w as f32, h as f32],
            t: ctx.t as f32,
            dt: ctx.dt as f32,
            seed: self.seed,
            _pad: [0.0; 3],
            params,
            extra: scene.as_ref().map_or([0.0; MAX_EXTRA], |s| s.extra),
            palette: std::array::from_fn(|i| {
                let c = scene.as_ref().and_then(|s| s.palette.as_ref()).and_then(|p| p.colours().get(i));
                c.map_or([0.0; 4], |c| [c.r, c.g, c.b, 1.0])
            }),
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
        match scene {
            Some(Scene { palette: Some(palette), dither, .. }) => palette.map(&frame, Dither::BlueNoise, dither),
            _ => frame,
        }
    }
}
