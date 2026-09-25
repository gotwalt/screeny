//! Test patterns.
//!
//! These are the bench patterns as much as they are a demo: each one makes a
//! specific class of mistake visible. They are also the encoder's adversarial
//! corpus, because between them they cover both extremes of what the codecs
//! find easy - `bars` has eight colours and compresses to nothing, `gradient`
//! has two thousand and compresses to nothing.

use screeny_proto::{H, W};

use crate::color::{lin_to_srgb8, SRGB_TO_LIN};
use crate::frame::{Frame, FrameSource, FrameTime};

/// One of the built-in patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Eight saturated vertical bars over a grey ramp: colour, and whether the
    /// panel's channels are wired the way we think.
    Bars,
    /// Grey ramps, one linear in code value and one linear in light. The
    /// gamma LUT is right when the second looks evenly spaced.
    Grey,
    /// A smooth two-dimensional RGB gradient. Banding, dither structure, and
    /// the codec's worst case for distinct colours.
    Gradient,
    /// A large "F" with distinct corner markers: rotation and mirroring.
    F,
    /// One-pixel checkerboard in the middle, coarser blocks around it.
    /// Ghosting, row driver crosstalk, and the LZ coder's worst case.
    Checker,
    /// A bright bar sweeping across and down. Tearing, latency, dropped
    /// frames - anything temporal.
    Sweep,
    /// A **held** ramp across the bottom of the range, sRGB 0..=70, warm
    /// monochrome over neutral grey. Card 248's bench pattern: how many dark
    /// shades the panel can tell apart, and whether any of them blink.
    Dark,
}

impl Pattern {
    /// Every pattern, in the order `screeny pattern --list` prints them.
    pub const ALL: [Pattern; 7] = [
        Pattern::Bars,
        Pattern::Grey,
        Pattern::Gradient,
        Pattern::F,
        Pattern::Checker,
        Pattern::Sweep,
        Pattern::Dark,
    ];

    /// The name used on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Pattern::Bars => "bars",
            Pattern::Grey => "grey",
            Pattern::Gradient => "gradient",
            Pattern::F => "f",
            Pattern::Checker => "checker",
            Pattern::Sweep => "sweep",
            Pattern::Dark => "dark",
        }
    }

    /// What it is for.
    #[must_use]
    pub fn blurb(self) -> &'static str {
        match self {
            Pattern::Bars => "colour bars over a grey ramp",
            Pattern::Grey => "grey ramps, linear in code and linear in light",
            Pattern::Gradient => "smooth 2D RGB gradient (many colours)",
            Pattern::F => "big F with corner markers: orientation",
            Pattern::Checker => "1px checkerboard and coarser blocks",
            Pattern::Sweep => "moving bar: tearing, latency, drops",
            Pattern::Dark => "held dark ramp, sRGB 0-70, warm over neutral",
        }
    }

    /// Parse a command-line name.
    #[must_use]
    pub fn from_name(s: &str) -> Option<Pattern> {
        Pattern::ALL.into_iter().find(|p| p.name() == s)
    }

    /// True if the pattern changes from frame to frame.
    #[must_use]
    pub fn animated(self) -> bool {
        matches!(self, Pattern::Sweep)
    }

    /// Render frame `t` of this pattern.
    pub fn render_at(self, t: u64, out: &mut Frame) {
        match self {
            Pattern::Bars => bars(out),
            Pattern::Grey => grey(out),
            Pattern::Gradient => gradient(out),
            Pattern::F => letter_f(out),
            Pattern::Checker => checker(out),
            Pattern::Sweep => sweep(t, out),
            Pattern::Dark => dark(out),
        }
    }

    /// A single frame of this pattern.
    #[must_use]
    pub fn frame(self, t: u64) -> Frame {
        let mut f = Frame::black();
        self.render_at(t, &mut f);
        f
    }
}

impl FrameSource for Pattern {
    fn render(&mut self, t: FrameTime, out: &mut Frame) -> bool {
        self.render_at(t.index, out);
        true
    }
    fn name(&self) -> &'static str {
        Pattern::name(*self)
    }
}

const BAR_COLOURS: [[u8; 3]; 8] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
    [0, 0, 0],
];

fn bars(f: &mut Frame) {
    for y in 0..H {
        for x in 0..W {
            let c = if y < 24 {
                BAR_COLOURS[x * 8 / W]
            } else {
                // A 16-step grey ramp under the bars, so a single frame shows
                // both colour and tone.
                let v = ((x / 4) * 255 / 15) as u8;
                [v, v, v]
            };
            f.set(x, y, c);
        }
    }
}

fn grey(f: &mut Frame) {
    let t = &*SRGB_TO_LIN;
    let _ = t;
    for y in 0..H {
        for x in 0..W {
            let c = if y < H / 2 {
                // Linear in code value: even steps in sRGB.
                let v = (x * 255 / (W - 1)) as u8;
                [v, v, v]
            } else {
                // Linear in light: even steps in emitted luminance. On a panel
                // with a correct gamma LUT this is the one that looks like a
                // smooth ramp to the eye at the dark end.
                let v = lin_to_srgb8(x as f32 / (W - 1) as f32);
                [v, v, v]
            };
            f.set(x, y, c);
        }
    }
}

fn gradient(f: &mut Frame) {
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            f.set(
                x,
                y,
                [
                    (fx * 255.0 + 0.5) as u8,
                    (fy * 255.0 + 0.5) as u8,
                    (((1.0 - fx) * 0.5 + (1.0 - fy) * 0.5) * 255.0 + 0.5) as u8,
                ],
            );
        }
    }
}

/// A blocky "F" that is asymmetric in both axes, so a rotated or mirrored
/// panel is obvious at a glance, plus four different corner markers.
fn letter_f(f: &mut Frame) {
    const BG: [u8; 3] = [0, 0, 40];
    const FG: [u8; 3] = [255, 255, 255];
    for y in 0..H {
        for x in 0..W {
            f.set(x, y, BG);
        }
    }
    // Stem, top arm, middle arm. Box is x 22..42, y 4..28.
    let (x0, y0) = (22usize, 4usize);
    for y in 0..24 {
        for x in 0..5 {
            f.set(x0 + x, y0 + y, FG);
        }
    }
    for x in 0..18 {
        for y in 0..5 {
            f.set(x0 + x, y0 + y, FG);
        }
    }
    for x in 0..12 {
        for y in 0..4 {
            f.set(x0 + x, y0 + 10 + y, FG);
        }
    }
    // Corner markers: red top-left, green top-right, blue bottom-left,
    // yellow bottom-right.
    let corners: [([u8; 3], usize, usize); 4] = [
        ([255, 0, 0], 0, 0),
        ([0, 255, 0], W - 3, 0),
        ([0, 0, 255], 0, H - 3),
        ([255, 255, 0], W - 3, H - 3),
    ];
    for (c, cx, cy) in corners {
        for dy in 0..3 {
            for dx in 0..3 {
                f.set(cx + dx, cy + dy, c);
            }
        }
    }
}

fn checker(f: &mut Frame) {
    for y in 0..H {
        for x in 0..W {
            // Coarse 8x8 checker everywhere, replaced by a 1x1 checker in the
            // middle third where the eye can judge it against the coarse one.
            let coarse = ((x / 8) + (y / 8)) % 2 == 0;
            let fine = (x + y) % 2 == 0;
            let on = if (16..48).contains(&x) && (8..24).contains(&y) {
                fine
            } else {
                coarse
            };
            f.set(x, y, if on { [255; 3] } else { [0; 3] });
        }
    }
}

fn sweep(t: u64, f: &mut Frame) {
    // 2 s horizontal cycle at 30 fps, 4 s vertical, so the two never line up.
    let hx = (t % 60) as usize * W / 60;
    let vy = (t % 120) as usize * H / 120;
    for y in 0..H {
        for x in 0..W {
            let mut c = [0u8, 0, 24];
            let dx = (x + W - hx) % W;
            if dx < 3 {
                let v = 255 - (dx as u8) * 80;
                c = [v, v, v];
            }
            let dy = (y + H - vy) % H;
            if dy < 2 {
                c = [255, 96, 0];
            }
            f.set(x, y, c);
        }
    }
}

// ---------------------------------------------------------------------------
// Card 248: the dark end, held still
// ---------------------------------------------------------------------------

/// The top of the dark ramp. Card 248 asks for sRGB 0..=70: everything below
/// this is where the panel's 63 duty levels are coarser than the eight-bit
/// hand-over, where the temporal dither is doing all the work, and therefore
/// where it blinks if it is going to.
const DARK_TOP: u8 = 70;

/// Steps across the panel. Sixteen of them on a 64-column panel is four pixels
/// each, which is wide enough to count from a few feet away and narrow enough
/// that two adjacent steps can be compared without moving your eyes.
const DARK_STEPS: usize = 16;

/// The sRGB code of step `k`: an even ramp in *code*, black to [`DARK_TOP`].
///
/// Even in code and not in light, deliberately. The question this pattern
/// answers is "how many of these can you tell apart", and an even ramp in code
/// is the one whose answer is directly comparable between two firmware builds.
fn dark_step(k: usize) -> u8 {
    let top = DARK_TOP as usize;
    let last = DARK_STEPS - 1;
    ((2 * k * top + last) / (2 * last)) as u8
}

/// A warm monochrome of `v`: roughly 2700 K, held in the same ratios all the
/// way down so the three channels land on *different* sub-level remainders.
/// A dither that blinks per channel shows up here as a colour shimmer that the
/// neutral half below cannot show.
fn warm(v: u8) -> [u8; 3] {
    let v16 = u16::from(v);
    [
        v,
        ((v16 * 205 + 128) >> 8) as u8,
        ((v16 * 141 + 128) >> 8) as u8,
    ]
}

/// A **held** dark ramp: warm monochrome on the top half, neutral grey on the
/// bottom, sixteen four-pixel steps from black to sRGB 70.
///
/// Held is the whole point (`animated()` is false for it). Card 248 is about a
/// flicker the author sees on a *static* picture from a few feet away; anything
/// that moves by itself hides exactly that. What it is for:
///
/// * **Count the steps.** With `output.panel: bit_planes` the bottom few
///   collapse together. With the device dither on there should be more of them.
/// * **Watch them hold still.** Any step that shimmers, breathes or blinks is
///   a sub-level remainder being spent too slowly.
/// * **Compare the halves.** The warm half's three channels sit on different
///   remainders; if it shimmers in colour while the grey half is steady, the
///   fault is per channel rather than per pixel.
fn dark(f: &mut Frame) {
    for y in 0..H {
        for x in 0..W {
            let v = dark_step(x * DARK_STEPS / W);
            f.set(x, y, if y < H / 2 { warm(v) } else { [v, v, v] });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held means held: the same frame at any `t`, and not animated.
    #[test]
    fn the_dark_ramp_does_not_move() {
        assert!(!Pattern::Dark.animated());
        let a = Pattern::Dark.frame(0);
        for t in [1u64, 2, 29, 30, 31, 1_000_000] {
            assert_eq!(a.as_bytes(), Pattern::Dark.frame(t).as_bytes(), "t {t}");
        }
    }

    /// The ramp is the bottom of the range, evenly and monotonically, with no
    /// step repeated - otherwise "how many can you tell apart" has a different
    /// answer before it ever reaches the panel.
    #[test]
    fn the_dark_ramp_is_sixteen_rising_steps_to_seventy() {
        let steps: Vec<u8> = (0..DARK_STEPS).map(dark_step).collect();
        assert_eq!(steps.len(), 16);
        assert_eq!(steps[0], 0);
        assert_eq!(*steps.last().unwrap(), DARK_TOP);
        for w in steps.windows(2) {
            assert!(w[1] > w[0], "{steps:?} must rise");
        }
        // Even in code: no two gaps differ by more than one.
        let gaps: Vec<i32> = steps
            .windows(2)
            .map(|w| i32::from(w[1]) - i32::from(w[0]))
            .collect();
        assert!(
            gaps.iter().max().unwrap() - gaps.iter().min().unwrap() <= 1,
            "{gaps:?}"
        );
    }

    /// Four pixels per step, and the same value all the way down each half.
    #[test]
    fn each_step_is_four_pixels_wide() {
        let f = Pattern::Dark.frame(0);
        assert_eq!(W / DARK_STEPS, 4);
        for k in 0..DARK_STEPS {
            let want_grey = [dark_step(k); 3];
            let want_warm = warm(dark_step(k));
            for dx in 0..4 {
                let x = k * 4 + dx;
                for y in 0..H {
                    let got = f.get(x, y);
                    let want = if y < H / 2 { want_warm } else { want_grey };
                    assert_eq!(got, want, "({x},{y}) step {k}");
                }
            }
        }
    }

    /// The top half is warm and the bottom half is neutral, and the warm half
    /// really does separate its channels wherever there is light to separate.
    #[test]
    fn the_top_half_is_warm_and_the_bottom_half_is_neutral() {
        let f = Pattern::Dark.frame(0);
        for x in 0..W {
            let [r, g, b] = f.get(x, 0);
            assert!(r >= g && g >= b, "x {x}: {r},{g},{b} is not warm");
            let [gr, gg, gb] = f.get(x, H - 1);
            assert!(gr == gg && gg == gb, "x {x}: {gr},{gg},{gb} is not neutral");
            assert_eq!(gr, r, "the two halves ramp together at x {x}");
        }
        // The brightest step pulls the channels well apart.
        assert_eq!(warm(DARK_TOP), [70, 56, 39]);
        // ... and even a step near the floor does, which is the case the
        // per-channel dither has to get right.
        assert_eq!(warm(14), [14, 11, 8]);
    }
}
