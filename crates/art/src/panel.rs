//! Panel model: what the LEDs can actually show.
//!
//! PROVISIONAL, per the brief: 64 linear PWM levels per channel today, fewer
//! when the firmware dims by scaling. The level count is a parameter so art can
//! be checked at 32 (and worse). Reconcile here when the firmware settles.

use crate::color::Rgb;

pub const NATIVE_LEVELS: u32 = 64;

#[derive(Clone, Copy, Debug)]
pub struct Panel {
    pub levels: u32,
}

impl Default for Panel {
    fn default() -> Self {
        Panel { levels: NATIVE_LEVELS }
    }
}

impl Panel {
    pub fn new(levels: u32) -> Self {
        Panel { levels: levels.clamp(2, 256) }
    }

    fn top(self) -> f32 {
        (self.levels - 1) as f32
    }

    /// Nearest displayable value for one linear channel. `bias` is a dither
    /// offset in units of one level, in -0.5..0.5.
    pub fn quantise(self, v: f32, bias: f32) -> f32 {
        let top = self.top();
        (v.clamp(0.0, 1.0) * top + bias).round().clamp(0.0, top) / top
    }

    pub fn snap(self, c: Rgb, bias: f32) -> Rgb {
        Rgb::new(self.quantise(c.r, bias), self.quantise(c.g, bias), self.quantise(c.b, bias))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::linear_to_srgb8;

    /// The first rows of the level table in brief section 2.1.
    #[test]
    fn levels_match_the_brief() {
        let want = [0u8, 34, 50, 62, 71, 80, 87, 94, 100, 106];
        for (k, w) in want.iter().enumerate() {
            assert_eq!(linear_to_srgb8(k as f32 / 63.0), *w, "level {k}");
        }
        assert_eq!(linear_to_srgb8(62.0 / 63.0), 253);
    }

    #[test]
    fn dark_srgb_is_off() {
        let p = Panel::default();
        assert_eq!(p.quantise(crate::color::srgb8_to_linear(21), 0.0), 0.0);
        assert!(p.quantise(crate::color::srgb8_to_linear(23), 0.0) > 0.0);
    }
}
