//! The panel model: what the LEDs actually do to a frame.
//!
//! `docs/design/generative-art-brief.md` section 5: sRGB -> linear ->
//! quantise each channel to a fixed number of levels -> back to sRGB, with
//! brightness applied in linear light because that is where a duty cycle
//! lives. The firmware does the same thing with a gamma/brightness LUT
//! (`docs/design/architecture.md`), and does it with the same loss: scaling
//! before quantising is what card 020 exists to fix, so the simulator models
//! the unfixed version.
//!
//! The result is deliberately *not* what the sender sent. Anything that wants
//! bit-exact pixels - every test in this crate, for instance - reads
//! [`crate::Snapshot::decoded`] instead.

use screeny_proto::{Rgb888Frame, NBYTES};

use crate::config::PanelModel;

/// sRGB 8-bit to linear, 0.0..=1.0.
#[must_use]
pub fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear 0.0..=1.0 back to sRGB 8-bit.
#[must_use]
pub fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u8
}

/// The 256-entry lookup the firmware would burn into flash.
///
/// One per brightness value, rebuilt whenever brightness or the level count
/// changes - which, on a real device, is only when someone sends
/// `SET_BRIGHTNESS`.
#[derive(Debug, Clone)]
pub struct Lut {
    table: [u8; 256],
    model: PanelModel,
    brightness: u8,
}

impl Lut {
    /// Build the lookup for one brightness.
    #[must_use]
    pub fn new(model: &PanelModel, brightness: u8) -> Self {
        let levels = model.levels.max(2) as f32;
        let top = levels - 1.0;
        let scale = brightness as f32 / 255.0;
        let mut table = [0u8; 256];
        for (v, slot) in table.iter_mut().enumerate() {
            let lin = srgb_to_linear(v as u8) * scale;
            // The panel has `levels` duty steps per channel and nothing in
            // between; round to the nearest one.
            let q = (lin * top + 0.5).floor().clamp(0.0, top) / top;
            *slot = linear_to_srgb(q);
        }
        Lut {
            table,
            model: *model,
            brightness,
        }
    }

    /// True if this lookup is still the right one.
    #[must_use]
    pub fn matches(&self, model: &PanelModel, brightness: u8) -> bool {
        self.model == *model && self.brightness == brightness
    }

    /// Map one channel.
    #[must_use]
    pub fn map(&self, v: u8) -> u8 {
        self.table[v as usize]
    }

    /// Map a whole frame.
    pub fn apply(&self, src: &Rgb888Frame, dst: &mut Rgb888Frame) {
        for i in 0..NBYTES {
            dst[i] = self.table[src[i] as usize];
        }
    }
}

/// Run one frame through the panel model. Builds the lookup each time, which
/// is fine at the once-per-displayed-frame rate this is called at.
pub fn apply(model: &PanelModel, brightness: u8, src: &Rgb888Frame, dst: &mut Rgb888Frame) {
    Lut::new(model, brightness).apply(src, dst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trips_through_linear() {
        for v in 0..=255u8 {
            assert_eq!(linear_to_srgb(srgb_to_linear(v)), v, "channel {v}");
        }
    }

    #[test]
    fn full_brightness_black_and_white_are_exact() {
        let lut = Lut::new(&PanelModel { levels: 64 }, 255);
        assert_eq!(lut.map(0), 0);
        assert_eq!(lut.map(255), 255);
    }

    #[test]
    fn quantisation_is_visible_and_monotonic() {
        let lut = Lut::new(&PanelModel { levels: 64 }, 255);
        let distinct: std::collections::BTreeSet<u8> = (0..=255u8).map(|v| lut.map(v)).collect();
        assert_eq!(distinct.len(), 64, "one output per panel duty step");
        let mut prev = 0u8;
        for v in 0..=255u8 {
            let m = lut.map(v);
            assert!(m >= prev, "LUT must be monotonic at {v}");
            prev = m;
        }
    }

    #[test]
    fn thirty_two_levels_is_coarser_than_sixty_four() {
        let a = Lut::new(&PanelModel { levels: 32 }, 255);
        let b = Lut::new(&PanelModel { levels: 64 }, 255);
        let n = |l: &Lut| {
            (0..=255u8)
                .map(|v| l.map(v))
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        };
        assert_eq!(n(&a), 32);
        assert_eq!(n(&b), 64);
    }

    #[test]
    fn brightness_darkens_everything_and_zero_is_black() {
        let full = Lut::new(&PanelModel::default(), 255);
        let half = Lut::new(&PanelModel::default(), 128);
        let off = Lut::new(&PanelModel::default(), 0);
        for v in 0..=255u8 {
            assert!(half.map(v) <= full.map(v), "half is never brighter at {v}");
            assert_eq!(off.map(v), 0, "brightness 0 is black at {v}");
        }
        assert!(half.map(255) < 255);
    }
}
