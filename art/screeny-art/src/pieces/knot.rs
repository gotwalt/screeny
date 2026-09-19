//! A torus knot as real geometry: vertex and index buffers, a camera, a depth
//! buffer. The template for mesh-based 3D pieces, where `lattice` is the
//! template for shader-only ones.

use crate::dither::Dither;
use crate::frame::Frame;
use crate::gpu::mat::{self, Mat4};
use crate::gpu::{Gpu, Offscreen, COLOR_FORMAT, COMMON_WGSL, DEPTH_FORMAT};
use crate::palette::Palette;
use crate::piece::{param, Ctx, ParamSpec, Piece, PieceDef};
use crate::rng::Rng;
use std::f32::consts::TAU;
use wgpu::util::DeviceExt;

pub const DEF: PieceDef = PieceDef {
    id: "knot",
    name: "Torus knot",
    blurb: "GPU, rasterized mesh with a depth buffer, then mapped onto a 31-colour designed palette so it is sent exactly.",
    params: PARAMS,
    make,
};

const PARAMS: &[ParamSpec] = &[
    param("speed", "Spin", 0.0, 2.0, 0.01, 0.3),
    param("distance", "Camera distance", 1.6, 6.0, 0.01, 2.45),
    param("tube", "Tube thickness", 0.05, 0.45, 0.005, 0.13),
    param("hue", "Hue", 0.0, 360.0, 1.0, 330.0),
    param("spread", "Hue spread", 0.0, 360.0, 1.0, 200.0),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 4.0),
    param("steps", "Palette steps (0 = continuous)", 0.0, 6.0, 1.0, 5.0),
    param("dither", "Palette dither", 0.0, 1.0, 0.01, 0.8),
];

/// Hues in the designed palette; with up to 6 lightness steps each, plus
/// black, that is 31 colours: always an exact frame.
const PALETTE_HUES: usize = 5;

const SEGMENTS: usize = 240;
const SIDES: usize = 20;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    centre: [f32; 3],
    normal: [f32; 3],
    along: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Scene {
    view_proj: Mat4,
    model: Mat4,
    eye: [f32; 4],
    look: [f32; 4],
}

struct Live {
    gpu: &'static Gpu,
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    scene: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    target: Offscreen,
}

struct Knot {
    /// Winds `p` times around the axis and `q` times through the hole.
    winding: (f32, f32),
    tilt: f32,
    live: Option<Option<Live>>,
}

fn make(seed: u64) -> Box<dyn Piece> {
    let mut rng = Rng::new(seed);
    let windings = [(2.0, 3.0), (3.0, 2.0), (2.0, 5.0), (3.0, 4.0), (3.0, 5.0)];
    let winding = windings[(rng.u64() % windings.len() as u64) as usize];
    Box::new(Knot { winding, tilt: rng.range(0.0, TAU), live: None })
}

/// Centre line of a (p, q) torus knot, scaled to roughly unit radius.
fn curve(u: f32, (p, q): (f32, f32)) -> [f32; 3] {
    let r = 0.5 * (2.0 + (q / p * u).cos());
    [r * u.cos() * 0.62, r * u.sin() * 0.62, (q / p * u).sin() * 0.5 * 0.62]
}

fn mesh(winding: (f32, f32)) -> (Vec<Vertex>, Vec<u16>) {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let add = |a: [f32; 3], b: [f32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let unit = |a: [f32; 3]| {
        let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
        [a[0] / l, a[1] / l, a[2] / l]
    };

    let mut vertices = Vec::with_capacity(SEGMENTS * SIDES);
    for i in 0..SEGMENTS {
        let u = i as f32 / SEGMENTS as f32 * winding.0 * TAU;
        let (here, ahead) = (curve(u, winding), curve(u + 0.01, winding));
        // A frame that depends only on position, so it closes up at the seam.
        let tangent = sub(ahead, here);
        let binormal = unit(cross(tangent, add(ahead, here)));
        let normal = unit(cross(binormal, tangent));
        for j in 0..SIDES {
            let (s, c) = (j as f32 / SIDES as f32 * TAU).sin_cos();
            vertices.push(Vertex {
                centre: here,
                normal: [0, 1, 2].map(|k| c * normal[k] + s * binormal[k]),
                along: i as f32 / SEGMENTS as f32,
            });
        }
    }

    let mut indices = Vec::with_capacity(SEGMENTS * SIDES * 6);
    let at = |i: usize, j: usize| ((i % SEGMENTS) * SIDES + j % SIDES) as u16;
    for i in 0..SEGMENTS {
        for j in 0..SIDES {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
            indices.extend_from_slice(&[a, b, d, b, c, d]);
        }
    }
    (vertices, indices)
}

impl Knot {
    fn open(&self, samples: u32) -> Option<Live> {
        let gpu = match Gpu::shared() {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("screeny-art: knot: {e}; rendering black");
                return None;
            }
        };
        let module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("knot"),
            source: wgpu::ShaderSource::Wgsl(format!("{COMMON_WGSL}\n{}", include_str!("knot.wgsl")).into()),
        });
        let pipeline = gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("knot"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32],
                })],
            },
            // No culling: the depth buffer sorts it out, and winding stops mattering.
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
                targets: &[Some(COLOR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

        let (vertices, indices) = mesh(self.winding);
        let buffer = |label, contents: &[u8], usage| {
            gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents, usage })
        };
        let scene = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene"),
            size: std::mem::size_of::<Scene>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: scene.as_entire_binding() }],
        });
        Some(Live {
            gpu,
            pipeline,
            vertices: buffer("knot vertices", bytemuck::cast_slice(&vertices), wgpu::BufferUsages::VERTEX),
            indices: buffer("knot indices", bytemuck::cast_slice(&indices), wgpu::BufferUsages::INDEX),
            index_count: indices.len() as u32,
            scene,
            bind_group,
            target: Offscreen::new(gpu, samples),
        })
    }
}

impl Piece for Knot {
    fn render(&mut self, ctx: &Ctx) -> Frame {
        let samples = ctx.get("samples") as u32;
        if self.live.is_none() {
            self.live = Some(self.open(samples));
        }
        let Some(Some(live)) = self.live.as_mut() else { return Frame::black() };
        if live.target.samples() != samples.clamp(1, 16) {
            live.target = Offscreen::new(live.gpu, samples);
        }

        let spin = ctx.t as f32 * ctx.get("speed");
        let distance = ctx.get("distance");
        let model = mat::mul(mat::rot_y(spin), mat::mul(mat::rot_x(spin * 0.61 + self.tilt), mat::rot_z(spin * 0.23)));
        let view = mat::translate(0.0, 0.0, -distance);
        let proj = mat::perspective(0.7, 2.0, 0.1, 20.0);
        let scene = Scene {
            view_proj: mat::mul(proj, view),
            model,
            eye: [0.0, 0.0, distance, 1.0],
            look: [ctx.get("tube"), ctx.get("hue"), ctx.get("spread"), 0.0],
        };
        live.gpu.queue.write_buffer(&live.scene, 0, bytemuck::bytes_of(&scene));

        let mut encoder = live.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = live.target.pass(&mut encoder);
            pass.set_pipeline(&live.pipeline);
            pass.set_bind_group(0, &live.bind_group, &[]);
            pass.set_vertex_buffer(0, live.vertices.slice(..));
            pass.set_index_buffer(live.indices.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..live.index_count, 0, 0..1);
        }
        let frame = live.target.finish(live.gpu, encoder);

        // Bring the continuous render inside the panel's colour budget: the
        // same hue arc and lightness range the shader uses, as a fixed palette.
        let steps = ctx.get("steps") as usize;
        if steps == 0 {
            return frame;
        }
        let (hue, spread) = (ctx.get("hue"), ctx.get("spread"));
        let hues: Vec<f32> = (0..PALETTE_HUES).map(|k| hue + spread * k as f32 / (PALETTE_HUES - 1) as f32).collect();
        Palette::ramps(&hues, steps, (0.42, 0.9), 0.23).map(&frame, Dither::BlueNoise, ctx.get("dither"))
    }
}
