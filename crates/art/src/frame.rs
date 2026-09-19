//! What a piece produces, and what gets handed to an output.

use crate::color::Rgb;

pub const W: usize = 64;
pub const H: usize = 32;
pub const N: usize = W * H;
/// Largest palette that is still an exact, lossless frame (brief section 2.3).
pub const MAX_PALETTE: usize = 32;

/// A frame as a piece makes it: linear light, not yet quantised.
///
/// Prefer `Indexed` for work you control. It goes to the panel exactly as made,
/// and animating the palette costs almost nothing.
#[derive(Clone, Debug)]
pub enum Frame {
    /// Row-major, top-left origin, `N` pixels.
    Linear(Vec<Rgb>),
    /// At most `MAX_PALETTE` colours plus `N` indices into them.
    Indexed { palette: Vec<Rgb>, indices: Vec<u8> },
}

impl Frame {
    pub fn black() -> Self {
        Frame::Linear(vec![Rgb::BLACK; N])
    }

    /// Render by point-sampling `f` on an `ss` x `ss` grid inside every pixel and
    /// box-filtering in linear light. `f` gets panel coordinates: x in 0..64,
    /// y in 0..32, so 1.0 is one LED pitch.
    ///
    /// This is what gives sub-pixel motion and anti-aliased edges for free; at
    /// 2048 pixels even `ss = 8` is cheap.
    pub fn supersample(ss: usize, mut f: impl FnMut(f32, f32) -> Rgb) -> Self {
        let ss = ss.clamp(1, 16);
        let inv = 1.0 / ss as f32;
        let norm = inv * inv;
        let mut px = Vec::with_capacity(N);
        for y in 0..H {
            for x in 0..W {
                let mut acc = Rgb::BLACK;
                for sy in 0..ss {
                    for sx in 0..ss {
                        let fx = x as f32 + (sx as f32 + 0.5) * inv;
                        let fy = y as f32 + (sy as f32 + 0.5) * inv;
                        acc = acc.add(f(fx, fy));
                    }
                }
                px.push(acc.scale(norm));
            }
        }
        Frame::Linear(px)
    }

    /// Pixel `i` as linear RGB, whichever representation this is.
    pub fn pixel(&self, i: usize) -> Rgb {
        match self {
            Frame::Linear(px) => px[i],
            Frame::Indexed { palette, indices } => {
                palette.get(indices[i] as usize).copied().unwrap_or(Rgb::BLACK)
            }
        }
    }

    /// Multiply every colour by `k`. On an indexed frame this touches only the
    /// palette, so the frame stays exact.
    pub fn scale(&mut self, k: f32) {
        let colours = match self {
            Frame::Linear(px) => px,
            Frame::Indexed { palette, .. } => palette,
        };
        for c in colours.iter_mut() {
            *c = c.scale(k);
        }
    }
}

/// A frame ready to hand over: 8-bit sRGB, already snapped to levels the panel
/// can show. Mirrors the two hand-over formats in brief section 5, and nothing
/// more; how it is encoded on the wire is the sender's business.
#[derive(Clone, Debug)]
pub struct WireFrame {
    /// `N * 3` bytes, row-major R,G,B. Always present.
    pub rgb: Vec<u8>,
    /// Present when the piece rendered indexed: sRGB palette and `N` indices.
    pub indexed: Option<(Vec<[u8; 3]>, Vec<u8>)>,
}
