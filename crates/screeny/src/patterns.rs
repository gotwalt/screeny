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
}

impl Pattern {
    /// Every pattern, in the order `screeny pattern --list` prints them.
    pub const ALL: [Pattern; 6] = [
        Pattern::Bars,
        Pattern::Grey,
        Pattern::Gradient,
        Pattern::F,
        Pattern::Checker,
        Pattern::Sweep,
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
