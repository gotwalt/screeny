//! Encoders. Host-side only: these may use f32, allocate, and be as slow as
//! they like. The matching decoders in `crate::dec` are the constrained half.

use crate::frame::Frame;

pub mod block;
pub mod hybrid;
pub mod lz;
pub mod pal;
pub mod quant;
pub mod ycocg;

/// Per-sequence encoder state. Codecs that want temporal stability (palette
/// reuse, mode hysteresis) read and update it; stateless codecs ignore it.
///
/// Nothing here is needed to *decode*: a lost packet costs one frame and
/// nothing more. That is a hard requirement from the loss model.
#[derive(Default)]
pub struct EncCtx {
    pub frame_idx: usize,
    pub prev_pal: Option<Vec<[u8; 3]>>,
    pub prev_mode: Option<u8>,
}

pub trait Codec: Sync {
    fn name(&self) -> &'static str;
    fn note(&self) -> &'static str;
    /// Encode one frame into at most `budget` bytes. Must never exceed it.
    fn encode(&self, f: &Frame, budget: usize, ctx: &mut EncCtx) -> Vec<u8>;
}

/// The dither strategy shared by the palette and YCoCg codecs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dither {
    None,
    /// 8x8 Bayer, identical every frame: spatially noisy but temporally still.
    Ordered,
    /// 8x8 Bayer with a per-frame golden-ratio phase offset, so the *time*
    /// average over a few frames lands closer to the true colour.
    OrderedTemporal,
    /// Floyd-Steinberg, serpentine, in linear light.
    ErrorDiffusion,
}

/// 8x8 Bayer matrix normalised to [0,1).
pub const BAYER8: [[f32; 8]; 8] = {
    let raw: [[u8; 8]; 8] = [
        [0, 32, 8, 40, 2, 34, 10, 42],
        [48, 16, 56, 24, 50, 18, 58, 26],
        [12, 44, 4, 36, 14, 46, 6, 38],
        [60, 28, 52, 20, 62, 30, 54, 22],
        [3, 35, 11, 43, 1, 33, 9, 41],
        [51, 19, 59, 27, 49, 17, 57, 25],
        [15, 47, 7, 39, 13, 45, 5, 37],
        [63, 31, 55, 23, 61, 29, 53, 21],
    ];
    let mut out = [[0f32; 8]; 8];
    let mut y = 0;
    while y < 8 {
        let mut x = 0;
        while x < 8 {
            out[y][x] = (raw[y][x] as f32 + 0.5) / 64.0;
            x += 1;
        }
        y += 1;
    }
    out
};

/// Dither threshold for pixel (x,y) of frame `t`.
#[inline]
pub fn threshold(d: Dither, x: usize, y: usize, t: usize) -> f32 {
    let base = BAYER8[y & 7][x & 7];
    match d {
        Dither::OrderedTemporal => {
            let v = base + t as f32 * 0.618_034;
            v - v.floor()
        }
        _ => base,
    }
}

/// Assemble the full roster of codecs the harness measures.
pub fn roster() -> Vec<Box<dyn Codec>> {
    use block::BlockCodec;
    use pal::PalCodec;
    use ycocg::YCoCgCodec;
    vec![
        Box::new(PalCodec::fixed16()),
        Box::new(PalCodec::fixed32()),
        Box::new(PalCodec::adaptive(16, Dither::None, false)),
        Box::new(PalCodec::adaptive(16, Dither::Ordered, false)),
        Box::new(PalCodec::adaptive(32, Dither::None, false)),
        Box::new(PalCodec::adaptive(32, Dither::Ordered, false)),
        Box::new(PalCodec::adaptive(32, Dither::ErrorDiffusion, false)),
        Box::new(PalCodec::adaptive(32, Dither::Ordered, true)),
        Box::new(PalCodec::adaptive(32, Dither::OrderedTemporal, true)),
        Box::new(BlockCodec::bc1()),
        Box::new(BlockCodec::bc1_i3()),
        Box::new(BlockCodec::bc1_e888()),
        Box::new(BlockCodec::blk42()),
        Box::new(BlockCodec::blk84_i3()),
        Box::new(block::BlockDual),
        Box::new(BlockCodec::cc4()),
        Box::new(BlockCodec::cc2_42()),
        Box::new(BlockCodec::cc2_44()),
        Box::new(YCoCgCodec::c420()),
        Box::new(YCoCgCodec::c410()),
        Box::new(lz::PalLzCodec::new()),
        Box::new(hybrid::Hybrid::new()),
    ]
}

/// Utility used by several encoders: emitted linear light of an sRGB8 pixel.
pub fn lin(c: [u8; 3]) -> [f32; 3] {
    let t = &*crate::color::SRGB_TO_LIN;
    [t[c[0] as usize], t[c[1] as usize], t[c[2] as usize]]
}

pub fn lin_frame(f: &Frame) -> Vec<[f32; 3]> {
    (0..crate::frame::NPIX).map(|p| lin(f.at(p))).collect()
}
