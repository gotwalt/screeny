//! The panel model: what the LEDs actually emit for an sRGB8 frame.
//!
//! The panel modulates each channel in **linear light** with `levels` steps
//! (64 at the measured 6 bitplanes / 154 Hz; card 020 may change how dimming
//! works, so the level count is a parameter and every piece is checked at 32
//! as well). See `docs/design/generative-art-brief.md` section 2.1.

use crate::color::{lin_to_srgb8, SRGB_TO_LIN};

#[derive(Clone, Copy, Debug)]
pub struct Panel {
    pub levels: u32,
}

/// What card 001 measured: 6 bitplanes, 64 linear levels per channel.
pub const NOMINAL: Panel = Panel { levels: 64 };
/// A dim room today: brightness comes out of bit depth (brief section 2.1).
pub const DIM: Panel = Panel { levels: 32 };

impl Panel {
    pub const fn new(levels: u32) -> Self {
        Self { levels }
    }

    /// Emitted linear light (0..1) for one sRGB8 code value.
    pub fn emit1(&self, v: u8) -> f32 {
        let max = (self.levels - 1) as f32;
        (SRGB_TO_LIN[v as usize] * max).round() / max
    }

    /// Quantise a linear value to the nearest emittable level.
    pub fn quant_lin(&self, v: f32) -> f32 {
        let max = (self.levels - 1) as f32;
        (v.clamp(0.0, 1.0) * max).round() / max
    }

    pub fn emit(&self, c: [u8; 3]) -> [f32; 3] {
        [self.emit1(c[0]), self.emit1(c[1]), self.emit1(c[2])]
    }

    /// The sRGB8 code that displays what the panel would emit for `c`. This is
    /// the transform the preview applies before drawing dots.
    pub fn round_trip(&self, c: [u8; 3]) -> [u8; 3] {
        let e = self.emit(c);
        [lin_to_srgb8(e[0]), lin_to_srgb8(e[1]), lin_to_srgb8(e[2])]
    }

    /// Snap an sRGB8 colour to one the panel can show exactly. Palette design
    /// runs every entry through this, so a designed ramp has no wasted steps
    /// and no surprise colour casts from three channels rounding differently.
    pub fn snap(&self, c: [u8; 3]) -> [u8; 3] {
        self.round_trip(c)
    }
}
