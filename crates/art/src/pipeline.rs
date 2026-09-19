//! Piece frame in, hand-over frame + faithful preview + statistics out.
//!
//! ```text
//! piece -> limiter -> quantise to panel levels (ordered dither) -> WireFrame -> outputs
//!                                                              \-> lossy sim -> preview
//! ```

use crate::budget::{self, Encoding};
use crate::color::Rgb;
use crate::dither::Dither;
use crate::frame::{Frame, WireFrame, MAX_PALETTE, N, W};
use crate::limiter::{Limiter, LimiterSettings};
use crate::panel::{Panel, NATIVE_LEVELS};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Levels per channel the panel is assumed to have. 64 native; try 32.
    pub levels: u32,
    /// Dither used when a linear frame is quantised to those levels.
    pub dither: Dither,
    pub limiter: LimiterSettings,
    /// Preview only: show the frame as the panel would (quantised). Off shows
    /// the unquantised framebuffer, for comparison.
    pub panel_model: bool,
    /// Preview only: when a frame has more than 32 colours, show what a lossy
    /// adaptive-palette encode might do to it.
    pub lossy_sim: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            levels: NATIVE_LEVELS,
            dither: Dither::default(),
            limiter: LimiterSettings::default(),
            panel_model: true,
            lossy_sim: true,
        }
    }
}

/// The four numbers from brief section 5, and what the limiter did.
#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub distinct_colours: u32,
    pub encoding: Encoding,
    pub encoded_bytes: u32,
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
}

#[derive(Default)]
pub struct Pipeline {
    pub settings: Settings,
    limiter: Limiter,
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

    /// `dt` is wall-clock seconds since the previous frame.
    pub fn process(&mut self, mut frame: Frame, dt: f64) -> Output {
        let s = self.settings;
        let panel = Panel::new(s.levels);
        if let Frame::Indexed { palette, .. } = &mut frame {
            palette.truncate(MAX_PALETTE);
        }

        let limited = self.limiter.apply(&s.limiter, &mut frame, dt as f32);

        let wire = match &frame {
            Frame::Linear(px) => {
                let mut rgb = Vec::with_capacity(N * 3);
                for (i, c) in px.iter().enumerate() {
                    let bias = s.dither.threshold(i % W, i / W);
                    rgb.extend_from_slice(&panel.snap(*c, bias).to_srgb8());
                }
                WireFrame { rgb, indexed: None }
            }
            Frame::Indexed { palette, indices } => {
                let pal: Vec<[u8; 3]> = palette.iter().map(|c| panel.snap(*c, 0.0).to_srgb8()).collect();
                let mut rgb = Vec::with_capacity(N * 3);
                for &i in indices {
                    rgb.extend_from_slice(pal.get(i as usize).unwrap_or(&[0, 0, 0]));
                }
                WireFrame { rgb, indexed: Some((pal, indices.clone())) }
            }
        };

        let distinct = budget::distinct_colours(&wire.rgb);
        let encoding = Encoding::for_colours(distinct);

        let preview = if s.panel_model {
            let mut p = wire.rgb.clone();
            if s.lossy_sim && encoding == Encoding::Lossy {
                budget::simulate_lossy(&mut p, panel, s.dither);
            }
            p
        } else {
            (0..N).flat_map(|i| frame.pixel(i).to_srgb8()).collect()
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
            stats: Stats {
                distinct_colours: distinct as u32,
                encoding,
                encoded_bytes: encoding.bytes(),
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

    #[test]
    fn indexed_frames_survive_exactly() {
        let palette: Vec<Rgb> = (0..8).map(|k| Rgb::splat(k as f32 / 63.0 * 4.0)).collect();
        let indices: Vec<u8> = (0..N).map(|i| (i % 8) as u8).collect();
        let mut settings = Settings::default();
        settings.limiter.enabled = false;
        let out = Pipeline::new(settings).process(Frame::Indexed { palette, indices }, 1.0 / 30.0);
        assert_eq!(out.stats.distinct_colours, 8);
        assert_eq!(out.stats.encoding, Encoding::Index4);
        assert_eq!(out.preview, out.wire.rgb);
    }

    #[test]
    fn gradients_go_lossy() {
        let f = linear_frame(|x, y| Rgb::new(x as f32 / 63.0, y as f32 / 31.0, 0.5));
        let out = Pipeline::default().process(f, 1.0 / 30.0);
        assert_eq!(out.stats.encoding, Encoding::Lossy);
        assert!(budget::distinct_colours(&out.preview) <= MAX_PALETTE);
    }
}
