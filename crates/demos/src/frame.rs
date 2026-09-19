//! Frame types and the `Piece` trait.
//!
//! The types themselves are `crates/proto`'s (card 016): `Frame` is a heap box
//! of [`Rgb888Frame`], the `[u8; 6144]` sRGB888 buffer a sender encodes and a
//! device decodes into, and [`Indexed`] is the owned form of proto's borrowed
//! [`IndexedFrame`], which [`Indexed::as_proto`] hands out without copying.
//! The geometry constants are proto's too, so there is one definition of what
//! 64x32 means.
//!
//! What stays here is the `Piece` trait and the supersampled linear-light
//! buffer pieces draw into, neither of which a receiver has any use for.

use std::ops::{Deref, DerefMut};
use std::time::Duration;

use screeny_proto::{IndexedFrame, Rgb888Frame, NBYTES};

pub use screeny_proto::{H, NPIX, W};

/// 64x32 sRGB888, row-major, top-left origin.
#[derive(Clone)]
pub struct Frame {
    pub px: Box<Rgb888Frame>,
}

impl Deref for Frame {
    type Target = Rgb888Frame;
    fn deref(&self) -> &Rgb888Frame {
        &self.px
    }
}

impl DerefMut for Frame {
    fn deref_mut(&mut self) -> &mut Rgb888Frame {
        &mut self.px
    }
}

impl Default for Frame {
    fn default() -> Self {
        Self::black()
    }
}

impl Frame {
    pub fn black() -> Self {
        Frame {
            px: Box::new([0u8; NBYTES]),
        }
    }

    /// Take ownership of an existing pixel array.
    pub fn from_pixels(px: Box<Rgb888Frame>) -> Self {
        Frame { px }
    }

    /// The raw pixels, as the encoder and `screeny_proto::decode` want them.
    pub fn as_bytes(&self) -> &Rgb888Frame {
        &self.px
    }

    /// The raw pixels, mutably.
    pub fn as_bytes_mut(&mut self) -> &mut Rgb888Frame {
        &mut self.px
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        let o = (y * W + x) * 3;
        [self.px[o], self.px[o + 1], self.px[o + 2]]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, c: [u8; 3]) {
        if x >= W || y >= H {
            return;
        }
        let o = (y * W + x) * 3;
        self.px[o] = c[0];
        self.px[o + 1] = c[1];
        self.px[o + 2] = c[2];
    }

    pub fn fill(&mut self, c: [u8; 3]) {
        for i in 0..NPIX {
            self.px[i * 3] = c[0];
            self.px[i * 3 + 1] = c[1];
            self.px[i * 3 + 2] = c[2];
        }
    }

    pub fn pixels(&self) -> impl Iterator<Item = [u8; 3]> + '_ {
        (0..NPIX).map(move |i| [self.px[i * 3], self.px[i * 3 + 1], self.px[i * 3 + 2]])
    }
}

/// Palette (<= 32 entries for the exact-on-the-wire `PAL5` path) + one index
/// per pixel.
#[derive(Clone)]
pub struct Indexed {
    pub palette: Vec<[u8; 3]>,
    pub indices: Box<[u8; NPIX]>,
}

impl Default for Indexed {
    fn default() -> Self {
        Indexed {
            palette: vec![[0, 0, 0]],
            indices: Box::new([0u8; NPIX]),
        }
    }
}

impl Indexed {
    /// Borrow this frame as proto's [`IndexedFrame`], which is what the
    /// encoder's exact `PAL4_LZ`/`PAL5` path takes. No copy: the two types are
    /// the owned and the borrowed form of the same thing.
    pub fn as_proto(&self) -> IndexedFrame<'_> {
        IndexedFrame {
            palette: &self.palette,
            indices: &self.indices,
        }
    }

    pub fn to_frame(&self) -> Frame {
        let mut f = Frame::black();
        for i in 0..NPIX {
            let c = self.palette[self.indices[i] as usize];
            f.px[i * 3] = c[0];
            f.px[i * 3 + 1] = c[1];
            f.px[i * 3 + 2] = c[2];
        }
        f
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, idx: u8) {
        if x < W && y < H {
            self.indices[y * W + x] = idx;
        }
    }
}

/// A piece of art: a pure function of elapsed wall-clock time (plus whatever
/// state it keeps for itself) to a complete frame. Never a frame counter: a
/// dropped frame must cause a skip, not a slowdown (brief section 4).
pub trait Piece {
    /// Render the frame for elapsed time `t`.
    fn render(&mut self, t: Duration, out: &mut Frame);

    /// Render the same moment as palette + indices, if the piece owns its
    /// palette. Frames produced this way go on the wire exactly as made.
    fn render_indexed(&mut self, _t: Duration, _out: &mut Indexed) -> bool {
        false
    }

    fn name(&self) -> &'static str;
}

/// A supersampled linear-light buffer. Pieces draw into this at `SS` times the
/// panel resolution and box-filter down, which is what makes sub-pixel motion
/// and anti-aliased edges possible at 64x32 (brief section 3).
pub struct LinBuf {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[f32; 3]>,
}

impl LinBuf {
    pub fn new(ss: usize) -> Self {
        LinBuf {
            w: W * ss,
            h: H * ss,
            px: vec![[0.0; 3]; W * ss * H * ss],
        }
    }

    pub fn clear(&mut self) {
        self.px.fill([0.0; 3]);
    }

    #[inline]
    pub fn add(&mut self, x: usize, y: usize, c: [f32; 3], a: f32) {
        if x >= self.w || y >= self.h {
            return;
        }
        let p = &mut self.px[y * self.w + x];
        p[0] += c[0] * a;
        p[1] += c[1] * a;
        p[2] += c[2] * a;
    }

    /// Box-filter down to 64x32 in linear light, then encode to sRGB8.
    pub fn resolve(&self, out: &mut Frame) {
        let sx = self.w / W;
        let sy = self.h / H;
        let n = (sx * sy) as f32;
        for y in 0..H {
            for x in 0..W {
                let mut acc = [0f32; 3];
                for j in 0..sy {
                    for i in 0..sx {
                        let p = self.px[(y * sy + j) * self.w + x * sx + i];
                        acc[0] += p[0];
                        acc[1] += p[1];
                        acc[2] += p[2];
                    }
                }
                out.set(
                    x,
                    y,
                    crate::color::lin_to_srgb8_3([acc[0] / n, acc[1] / n, acc[2] / n]),
                );
            }
        }
    }

    /// Box-filter down to 64x32, leaving the result in linear light.
    pub fn resolve_lin(&self, out: &mut [[f32; 3]; NPIX]) {
        let sx = self.w / W;
        let sy = self.h / H;
        let n = (sx * sy) as f32;
        for y in 0..H {
            for x in 0..W {
                let mut acc = [0f32; 3];
                for j in 0..sy {
                    for i in 0..sx {
                        let p = self.px[(y * sy + j) * self.w + x * sx + i];
                        acc[0] += p[0];
                        acc[1] += p[1];
                        acc[2] += p[2];
                    }
                }
                out[y * W + x] = [acc[0] / n, acc[1] / n, acc[2] / n];
            }
        }
    }
}
