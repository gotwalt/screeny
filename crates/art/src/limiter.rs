//! Global safety stage at the end of the pipeline (brief sections 4 and 7), so
//! pieces do not each have to remember the rules.
//!
//! Two limits, both applied as a single gain on the whole frame. A gain keeps
//! indexed frames exact, because only the palette changes.
//!
//! - **Average picture level cap.** The panel is USB powered; a mostly-lit frame
//!   is harsh and is where power limiting would bite.
//! - **Rise limiter.** Mean luminance (and, separately, mean red) may only rise
//!   so fast. A flash is a rise and a fall; if rises are slow, a fast flash
//!   cannot reach a large amplitude, whatever the piece does. Falls are left
//!   alone, so cutting to black is always instant.

use crate::frame::{Frame, N};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimiterSettings {
    pub enabled: bool,
    /// Cap on mean channel duty, 0..1.
    pub apl_cap: f32,
    /// Largest allowed rise of mean luminance, in full-scale units per second.
    /// At 2.0, black to full white takes half a second, and a 7.5 Hz strobe is
    /// held to about 13% swing.
    pub max_rise_per_s: f32,
}

impl Default for LimiterSettings {
    fn default() -> Self {
        LimiterSettings { enabled: true, apl_cap: 0.40, max_rise_per_s: 2.0 }
    }
}

/// Whole-frame means, all in linear light.
#[derive(Clone, Copy, Debug, Default)]
pub struct Means {
    pub duty: f32,
    pub luma: f32,
    pub red: f32,
}

impl Means {
    pub fn of(frame: &Frame) -> Self {
        let mut m = Means::default();
        for i in 0..N {
            let c = frame.pixel(i).clamp01();
            m.duty += c.duty();
            m.luma += c.luma();
            m.red += c.r;
        }
        let k = 1.0 / N as f32;
        Means { duty: m.duty * k, luma: m.luma * k, red: m.red * k }
    }

    fn scale(self, k: f32) -> Self {
        Means { duty: self.duty * k, luma: self.luma * k, red: self.red * k }
    }
}

#[derive(Clone, Debug)]
pub struct Limiter {
    apl_gain: f32,
    prev: Means,
}

impl Default for Limiter {
    fn default() -> Self {
        Limiter { apl_gain: 1.0, prev: Means::default() }
    }
}

pub struct Limited {
    /// Means of the frame as the piece made it.
    pub input: Means,
    /// Means of the frame after limiting.
    pub output: Means,
    pub gain: f32,
}

impl Limiter {
    pub fn apply(&mut self, s: &LimiterSettings, frame: &mut Frame, dt: f32) -> Limited {
        let input = Means::of(frame);
        if !s.enabled {
            self.apl_gain = 1.0;
            self.prev = input;
            return Limited { input, output: input, gain: 1.0 };
        }
        let dt = dt.clamp(1.0 / 120.0, 0.1);

        // APL: clamp down at once, recover slowly so the gain does not pump.
        let target = if input.duty > s.apl_cap { s.apl_cap / input.duty } else { 1.0 };
        self.apl_gain = if target < self.apl_gain {
            target
        } else {
            self.apl_gain + (target - self.apl_gain) * (1.0 - (-dt / 0.5).exp())
        };
        let mut gain = self.apl_gain;

        let rise = s.max_rise_per_s.max(0.05) * dt;
        let after = input.scale(gain);
        for (now, before) in [(after.luma, self.prev.luma), (after.red, self.prev.red)] {
            let allowed = before + rise;
            if now > allowed {
                gain = gain.min(self.apl_gain * allowed / now);
            }
        }

        if gain < 0.9999 {
            frame.scale(gain);
        }
        let output = input.scale(gain);
        self.prev = output;
        Limited { input, output, gain }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgb;

    fn flat(v: f32) -> Frame {
        Frame::Linear(vec![Rgb::splat(v); N])
    }

    #[test]
    fn strobe_is_held_down() {
        let s = LimiterSettings::default();
        let mut lim = Limiter::default();
        let mut peak = 0.0_f32;
        // 7.5 Hz full-field strobe: two frames white, two black.
        for i in 0..120 {
            let mut f = flat(if (i / 2) % 2 == 0 { 1.0 } else { 0.0 });
            peak = peak.max(lim.apply(&s, &mut f, 1.0 / 30.0).output.luma);
        }
        assert!(peak < 0.15, "strobe reached {peak}");
    }

    #[test]
    fn steady_white_settles_at_the_cap() {
        let s = LimiterSettings::default();
        let mut lim = Limiter::default();
        let mut out = 0.0;
        for _ in 0..60 {
            let mut f = flat(1.0);
            out = lim.apply(&s, &mut f, 1.0 / 30.0).output.duty;
        }
        assert!((out - s.apl_cap).abs() < 0.01, "settled at {out}");
    }

    #[test]
    fn quiet_frames_pass_untouched() {
        let mut lim = Limiter::default();
        let mut f = flat(0.05);
        for _ in 0..10 {
            f = flat(0.05);
            lim.apply(&LimiterSettings::default(), &mut f, 1.0 / 30.0);
        }
        assert_eq!(f.pixel(0), Rgb::splat(0.05));
    }
}
