//! The panel model: what the LEDs actually do to a frame.
//!
//! One implementation, in [`screeny_panel::model`] (card 016), shared with the
//! sender and the demos. `docs/design/generative-art-brief.md` section 5:
//! sRGB -> linear -> quantise each channel to a fixed number of levels -> back
//! to sRGB, with brightness applied in linear light because that is where a
//! duty cycle lives.
//!
//! The result is deliberately *not* what the sender sent. Anything that wants
//! bit-exact pixels - every test in this crate, for instance - reads
//! [`crate::Snapshot::decoded`] instead.
//!
//! What is local here is the translation from the simulator's [`PanelModel`]
//! configuration knob to the shared [`Panel`].

use screeny_panel::model::{Lut, Panel};
use screeny_proto::Rgb888Frame;

use crate::config::PanelModel;

pub use screeny_panel::color::{lin_to_srgb8, srgb_to_lin_f};
pub use screeny_panel::model::Lut as PanelLut;

/// The shared panel for a simulator configuration.
#[must_use]
pub fn panel_of(model: &PanelModel) -> Panel {
    Panel::levels(model.levels as u32)
}

/// sRGB 8-bit to linear, 0.0..=1.0.
#[must_use]
pub fn srgb_to_linear(v: u8) -> f32 {
    srgb_to_lin_f(v as f32 / 255.0)
}

/// Linear 0.0..=1.0 back to sRGB 8-bit.
#[must_use]
pub fn linear_to_srgb(c: f32) -> u8 {
    lin_to_srgb8(c)
}

/// Run one frame through the panel model. Builds the lookup each time, which
/// is fine at the once-per-displayed-frame rate this is called at.
pub fn apply(model: &PanelModel, brightness: u8, src: &Rgb888Frame, dst: &mut Rgb888Frame) {
    Lut::new(panel_of(model), brightness).apply(src, dst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_knob_maps_to_the_shared_panel() {
        assert_eq!(panel_of(&PanelModel { levels: 64 }), screeny_panel::NOMINAL);
        assert_eq!(panel_of(&PanelModel { levels: 32 }), screeny_panel::DIM);
    }

    #[test]
    fn brightness_is_applied_in_linear_light_and_zero_is_black() {
        let model = PanelModel::default();
        let full = Lut::new(panel_of(&model), 255);
        let half = Lut::new(panel_of(&model), 128);
        for v in 0..=255u8 {
            assert!(half.map(v) <= full.map(v), "half is never brighter at {v}");
        }
        assert_eq!(full.map(0), 0);
        assert_eq!(full.map(255), 255);
    }
}
