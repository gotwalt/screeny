//! GPU rendering for pieces, through wgpu.
//!
//! wgpu needs no window and no event loop, so this works the same on the engine
//! thread of the studio (Metal on a Mac) and headless on a Linux box (Vulkan, or
//! OpenGL ES over EGL where that is all the GPU offers; force one with
//! `WGPU_BACKEND=gl` or `=vulkan`).
//!
//! A GPU piece is an ordinary [`Piece`](crate::Piece). It draws into an
//! [`Offscreen`] target several times the panel's resolution, in linear light,
//! and `Offscreen::finish` reads that back and box-filters it down to a 64x32
//! [`Frame`]. Everything after that (limiter, dither, panel model) is shared
//! with CPU pieces.

mod fragment;
pub mod mat;

pub use fragment::{Scene, SceneFn, ShaderPiece, MAX_EXTRA};

use crate::color::Rgb;
use crate::frame::{Frame, H, N, W};
use std::sync::OnceLock;

/// Linear-light float colour. Blendable, and enough range for additive glow.
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// WGSL shared by every GPU piece: OKLCH, so shaders pick colours the same way
/// CPU pieces do, plus the usual small helpers. Shaders output linear light.
pub const COMMON_WGSL: &str = include_str!("common.wgsl");

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// The process-wide device, created on first use. The error is kept, so a
    /// machine without a usable GPU reports it once and pieces fall back to black.
    pub fn shared() -> Result<&'static Gpu, &'static str> {
        static GPU: OnceLock<Result<Gpu, String>> = OnceLock::new();
        GPU.get_or_init(Gpu::open).as_ref().map_err(|e| e.as_str())
    }

    fn open() -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle().with_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|e| format!("no GPU adapter: {e}"))?;
        let info = adapter.get_info();
        eprintln!("screeny-art: gpu={} backend={:?}", info.name, info.backend);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("screeny-art"),
            // Stay within what OpenGL ES 3 class hardware can do.
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| format!("GPU device: {e}"))?;
        Ok(Gpu { device, queue })
    }
}

/// A supersampled colour + depth target with read-back.
pub struct Offscreen {
    ss: u32,
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    readback: wgpu::Buffer,
}

impl Offscreen {
    /// `ss` is samples per axis per LED: 8 renders at 512x256.
    pub fn new(gpu: &Gpu, ss: u32) -> Self {
        let ss = ss.clamp(1, 16);
        let size = wgpu::Extent3d { width: W as u32 * ss, height: H as u32 * ss, depth_or_array_layers: 1 };
        let texture = |label, format, usage| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let color = texture(
            "offscreen colour",
            COLOR_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let depth = texture("offscreen depth", DEPTH_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("offscreen readback"),
            size: size.width as u64 * size.height as u64 * 8,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Offscreen {
            ss,
            color_view: color.create_view(&Default::default()),
            depth_view: depth.create_view(&Default::default()),
            color,
            readback,
        }
    }

    pub fn samples(&self) -> u32 {
        self.ss
    }

    pub fn size(&self) -> (u32, u32) {
        (W as u32 * self.ss, H as u32 * self.ss)
    }

    /// Begin a pass that clears to black (and far depth) and draws into this target.
    pub fn pass<'e>(&self, encoder: &'e mut wgpu::CommandEncoder) -> wgpu::RenderPass<'e> {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("offscreen"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_view,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            ..Default::default()
        })
    }

    /// Submit `encoder`, read the target back and box-filter it, in linear
    /// light, down to one value per LED.
    pub fn finish(&self, gpu: &Gpu, mut encoder: wgpu::CommandEncoder) -> Frame {
        let (w, h) = self.size();
        encoder.copy_texture_to_buffer(
            self.color.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: None },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([encoder.finish()]);

        let slice = self.readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        if let Err(e) = gpu.device.poll(wgpu::PollType::wait_indefinitely()) {
            eprintln!("screeny-art: gpu poll failed: {e}");
            return Frame::black();
        }
        let frame = match slice.get_mapped_range() {
            Ok(data) => self.downsample(bytemuck::cast_slice(&data)),
            Err(e) => {
                eprintln!("screeny-art: gpu read-back failed: {e}");
                Frame::black()
            }
        };
        self.readback.unmap();
        frame
    }

    fn downsample(&self, texels: &[u16]) -> Frame {
        let lut = half_lut();
        let ss = self.ss as usize;
        let stride = W * ss * 4;
        let norm = 1.0 / (ss * ss) as f32;
        let mut px = Vec::with_capacity(N);
        for y in 0..H {
            for x in 0..W {
                let mut acc = [0.0_f32; 3];
                for sy in 0..ss {
                    let row = (y * ss + sy) * stride + x * ss * 4;
                    for t in texels[row..row + ss * 4].chunks_exact(4) {
                        acc[0] += lut[t[0] as usize];
                        acc[1] += lut[t[1] as usize];
                        acc[2] += lut[t[2] as usize];
                    }
                }
                px.push(Rgb::new(acc[0] * norm, acc[1] * norm, acc[2] * norm));
            }
        }
        Frame::Linear(px)
    }
}

/// IEEE half -> f32 for every bit pattern, with negatives, NaN and infinity
/// mapped into the displayable range so one bad texel cannot poison a pixel.
fn half_lut() -> &'static [f32] {
    static LUT: OnceLock<Vec<f32>> = OnceLock::new();
    LUT.get_or_init(|| {
        (0..=u16::MAX)
            .map(|h| {
                let (exp, man) = ((h >> 10) & 0x1f, (h & 0x3ff) as f32);
                let v = match exp {
                    0 => man * 2f32.powi(-24),
                    31 => return if man == 0.0 && h >> 15 == 0 { 64.0 } else { 0.0 },
                    e => (1.0 + man / 1024.0) * 2f32.powi(e as i32 - 15),
                };
                if h >> 15 == 1 { 0.0 } else { v.min(64.0) }
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_decodes() {
        let lut = half_lut();
        assert_eq!(lut[0x0000], 0.0);
        assert_eq!(lut[0x3c00], 1.0);
        assert_eq!(lut[0x3800], 0.5);
        assert_eq!(lut[0x4000], 2.0);
        assert_eq!(lut[0xbc00], 0.0); // -1 clamps to black
        assert_eq!(lut[0x7e00], 0.0); // NaN
    }
}
