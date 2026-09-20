//! Piece frame in, hand-over frame + faithful preview + statistics out.
//!
//! ```text
//! piece -> limiter -> quantise to the panel's duty steps (ordered dither) -> WireFrame -> outputs
//!                                                              \-> meter -> encode
//!                                                                        -> decode -> preview
//! ```
//!
//! The preview is not a model of the codec any more: the frame is put through
//! the sender's own encoder and the firmware's own decoder, and what comes
//! back out is what the panel shows. See [`crate::meter`].

use crate::color::Rgb;
use crate::dither::Dither;
use crate::frame::{Frame, WireFrame, MAX_PALETTE, N, W};
use crate::limiter::{Limiter, LimiterSettings};
use crate::meter::{Measured, Meter};
use crate::panel::Panel;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Which panel a frame is quantised to and previewed through: the device,
    /// or the same panel without its temporal dither (card 102).
    ///
    /// This replaces `levels`, a number with 64, 32 and 16 to choose from. 32
    /// and 16 were "the panel when it is dimmed", which the device has not
    /// done since card 020 and never will again; 64 was the panel before its
    /// temporal dither. A state file that still names `levels` loads - the key
    /// is ignored and the studio says so in its `repaired` list.
    pub panel: Panel,
    /// Dither used when a linear frame is quantised to the panel's duty steps.
    /// The bias is one duty step, so this only bites in the dark end, where
    /// the panel is coarser than the 8-bit hand-over. See [`Panel::quantise`].
    pub dither: Dither,
    pub limiter: LimiterSettings,
    /// Preview only: show the frame as the panel would - quantised to its duty
    /// steps, and with the dark end collapsed onto the levels it really has.
    /// Off shows the unquantised framebuffer, for comparison.
    pub panel_model: bool,
    /// Preview only: show the frame after the real encoder and the real
    /// decoder have been round it, so codec damage is visible. Off shows the
    /// frame as it was handed over, which is the same picture whenever
    /// [`Stats::exact`].
    #[serde(alias = "lossy_sim")]
    pub codec_preview: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            panel: Panel::DEVICE,
            dither: Dither::default(),
            limiter: LimiterSettings::default(),
            panel_model: true,
            codec_preview: true,
        }
    }
}

/// The four numbers from brief section 5, and what the limiter did.
#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub distinct_colours: u32,
    /// Wire codec id the encoder chose for this frame.
    pub codec: u8,
    /// Real payload bytes, against [`crate::meter::PAYLOAD_BYTES`].
    pub encoded_bytes: u32,
    /// True when the panel will show these pixels and not an approximation.
    pub exact: bool,
    /// Average picture level (mean channel duty) after limiting, 0..1.
    pub apl: f32,
    /// The same, as the piece made it.
    pub apl_in: f32,
    /// Mean luminance after limiting.
    pub luma: f32,
    /// Change in mean luminance since the previous frame.
    pub dluma: f32,
    /// Largest `dluma` over the last two seconds.
    pub dluma_peak: f32,
    /// 1.0 when the limiter is idle.
    pub limiter_gain: f32,
}

pub struct Output {
    pub wire: WireFrame,
    /// `N * 3` sRGB bytes: what the panel is expected to show.
    pub preview: Vec<u8>,
    pub stats: Stats,
    /// The encoder's verdict on this frame, in full.
    pub measured: Measured,
}

#[derive(Default)]
pub struct Pipeline {
    pub settings: Settings,
    limiter: Limiter,
    meter: Meter,
    prev_luma: Option<f32>,
    dluma_window: VecDeque<(f64, f32)>,
    clock: f64,
}

impl Pipeline {
    pub fn new(settings: Settings) -> Self {
        Pipeline { settings, ..Default::default() }
    }

    /// Forget limiter and statistics history, e.g. when the piece changes.
    pub fn reset(&mut self) {
        *self = Pipeline::new(self.settings);
    }

    /// The meter, for pointing at a connected device's real budget and codec
    /// set (`link.limits()`), or for reading the last frame's decode.
    pub fn meter(&mut self) -> &mut Meter {
        &mut self.meter
    }

    /// `dt` is wall-clock seconds since the previous frame.
    pub fn process(&mut self, mut frame: Frame, dt: f64) -> Output {
        let s = self.settings;
        let panel = s.panel;
        if let Frame::Indexed { palette, .. } = &mut frame {
            palette.truncate(MAX_PALETTE);
        }

        let limited = self.limiter.apply(&s.limiter, &mut frame, dt as f32);

        let wire = match &frame {
            Frame::Linear(px) => {
                let mut rgb = Vec::with_capacity(N * 3);
                for (i, c) in px.iter().enumerate() {
                    let bias = s.dither.threshold(i % W, i / W);
                    rgb.extend_from_slice(&panel.snap8(*c, bias));
                }
                WireFrame { rgb, indexed: None }
            }
            Frame::Indexed { palette, indices } => {
                let pal: Vec<[u8; 3]> = palette.iter().map(|c| panel.snap8(*c, 0.0)).collect();
                let mut rgb = Vec::with_capacity(N * 3);
                for &i in indices {
                    rgb.extend_from_slice(pal.get(i as usize).unwrap_or(&[0, 0, 0]));
                }
                WireFrame { rgb, indexed: Some((pal, indices.clone())) }
            }
        };

        // Ask the encoder rather than estimating: this is the codec that will
        // carry the frame, its real size, and - once decoded - the exact
        // picture the panel will put up.
        let measured = self.meter.measure(&wire);

        let preview = if !s.panel_model {
            (0..N).flat_map(|i| frame.pixel(i).to_srgb8()).collect()
        } else {
            let mut px = if s.codec_preview { self.meter.decoded().to_vec() } else { wire.rgb.clone() };
            // The last thing that happens to a frame is the panel itself. Above
            // sRGB 38 this is the identity; below it, codes the device cannot
            // tell apart arrive on the level it really has. Card 102: without
            // it the preview was flattering the darks by three codes.
            panel.show(&mut px);
            px
        };

        self.clock += dt;
        let luma = limited.output.luma;
        let dluma = self.prev_luma.map_or(0.0, |p| (luma - p).abs());
        self.prev_luma = Some(luma);
        self.dluma_window.push_back((self.clock, dluma));
        while self.dluma_window.front().is_some_and(|(t, _)| self.clock - t > 2.0) {
            self.dluma_window.pop_front();
        }
        let dluma_peak = self.dluma_window.iter().fold(0.0_f32, |m, (_, d)| m.max(*d));

        Output {
            wire,
            preview,
            measured,
            stats: Stats {
                distinct_colours: measured.colours,
                codec: measured.codec,
                encoded_bytes: measured.bytes,
                exact: measured.exact,
                apl: limited.output.duty,
                apl_in: limited.input.duty,
                luma,
                dluma,
                dluma_peak,
                limiter_gain: limited.gain,
            },
        }
    }
}

/// Convenience for tests and tools: a linear frame from a closure over pixels.
pub fn linear_frame(mut f: impl FnMut(usize, usize) -> Rgb) -> Frame {
    Frame::Linear((0..N).map(|i| f(i % W, i / W)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An indexed frame reaches the panel as the piece drew it, and the
    /// preview - which is now the decoded datagram, not a copy of the
    /// framebuffer - says so.
    #[test]
    fn indexed_frames_survive_exactly() {
        let palette: Vec<Rgb> = (0..8).map(|k| Rgb::splat(k as f32 / 63.0 * 4.0)).collect();
        let indices: Vec<u8> = (0..N).map(|i| (i % 8) as u8).collect();
        let mut settings = Settings::default();
        settings.limiter.enabled = false;
        let out = Pipeline::new(settings).process(Frame::Indexed { palette, indices }, 1.0 / 30.0);
        assert_eq!(out.stats.distinct_colours, 8);
        assert!(out.stats.exact);
        assert_eq!(out.stats.codec, screeny_proto::dec::codec::PAL4_LZ);
        assert!(out.stats.encoded_bytes < 1464);
        assert_eq!(out.preview, out.wire.rgb);
    }

    /// A gradient cannot be sent exactly, and the preview shows the damage
    /// the real codec does rather than a model of it.
    #[test]
    fn gradients_go_lossy() {
        let f = linear_frame(|x, y| Rgb::new(x as f32 / 63.0, y as f32 / 31.0, 0.5));
        let out = Pipeline::default().process(f, 1.0 / 30.0);
        assert!(!out.stats.exact);
        assert!(out.stats.encoded_bytes <= crate::meter::PAYLOAD_BYTES);
        assert_ne!(out.preview, out.wire.rgb);
    }

    /// `codec_preview` off shows the handed-over frame instead. For an exact
    /// frame the two are the same picture, which is the point.
    #[test]
    fn the_codec_preview_can_be_turned_off() {
        let f = linear_frame(|x, y| Rgb::new(x as f32 / 63.0, y as f32 / 31.0, 0.5));
        let settings = Settings { codec_preview: false, ..Settings::default() };
        let out = Pipeline::new(settings).process(f, 1.0 / 30.0);
        assert_eq!(out.preview, out.wire.rgb);
        assert!(!out.stats.exact, "the statistics are still the real ones");
    }
}
