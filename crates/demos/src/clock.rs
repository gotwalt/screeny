//! Word clock: the time spelled out, in the spirit of the stock Tidbyt app
//! (`docs/research/000-bench-notes.md`) but rendered for what this panel is
//! good at.
//!
//! Everything here is a pure function of the wall clock. The transition state
//! is derived from the time itself (how long since the phrase last changed),
//! so any moment can be rendered on its own - which is what lets the tests
//! check all 288 five-minute slots, and what makes a dropped frame a skip
//! rather than a stutter.
//!
//! Colour budget: one black plus a 7-step ramp of the text colour plus a
//! 3-step ramp of the accent = 11 entries, so every frame is `PAL4_LZ`
//! exact on the wire (protocol-v1 section 4.3).

use crate::color::{lin_to_srgb8_3, oklch_to_lin};
use crate::font::{self, ROWS};
use crate::frame::{Frame, Indexed, Piece, H, NPIX, W};
use crate::panel::{Panel, NOMINAL};
use chrono::{Local, NaiveDateTime, TimeDelta, Timelike};
use std::time::Duration;

/// Text ramp steps (plus black).
const TEXT_LEVELS: usize = 7;
/// Accent ramp steps (plus black).
const ACCENT_LEVELS: usize = 3;
/// Seconds for a word to roll out and the replacement to roll in.
const ROLL_SECS: f32 = 0.66;
/// Extra delay per line, so the phrase turns over as a cascade.
const LINE_STAGGER: f32 = 0.07;

const HOURS: [&str; 12] = [
    "TWELVE", "ONE", "TWO", "THREE", "FOUR", "FIVE", "SIX", "SEVEN", "EIGHT", "NINE", "TEN",
    "ELEVEN",
];

/// The phrase for a rounded time, as up to three lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phrase {
    pub lines: Vec<&'static str>,
}

impl Phrase {
    pub fn text(&self) -> String {
        self.lines.join(" ")
    }
}

fn hour_word(h24: u32) -> &'static str {
    HOURS[(h24 % 12) as usize]
}

/// Round to the nearest five minutes and return (hour, minute) in 24h.
pub fn round5(h24: u32, m: u32) -> (u32, u32) {
    let slot = ((h24 * 60 + m + 2) / 5) % 288;
    let t = slot * 5;
    (t / 60, t % 60)
}

/// English phrasing for every five-minute slot.
///
/// Colloquial, like the stock app: minutes past up to the half hour, then
/// minutes till the next hour. Exact noon and midnight say so instead of
/// "twelve o'clock", which is the one place a word clock can be clearer than
/// the numbers it replaces.
pub fn phrase(h24: u32, m: u32) -> Phrase {
    let (h, m) = round5(h24, m);
    let lines = if m == 0 {
        match h {
            0 => vec!["MIDNIGHT"],
            12 => vec!["NOON"],
            _ => vec![hour_word(h), "O'CLOCK"],
        }
    } else if m <= 30 {
        let amount = match m {
            5 => "FIVE",
            10 => "TEN",
            15 => "QUARTER",
            20 => "TWENTY",
            25 => "TWENTY FIVE",
            _ => "HALF",
        };
        vec![amount, "PAST", hour_word(h)]
    } else {
        let amount = match 60 - m {
            5 => "FIVE",
            10 => "TEN",
            15 => "QUARTER",
            20 => "TWENTY",
            _ => "TWENTY FIVE",
        };
        vec![amount, "TILL", hour_word(h + 1)]
    };
    Phrase { lines }
}

/// One word, placed. Words are the unit of motion: at 11:40 -> 11:45 only
/// "TWENTY FIVE" leaves and "QUARTER" arrives; "TILL" and "TWELVE" hold still.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub word: &'static str,
    pub line: usize,
    pub x: i32,
    pub y: i32,
}

/// Top row of each line band, and the staircase indent, by line count.
fn line_geometry(n: usize) -> (Vec<i32>, Vec<i32>) {
    match n {
        1 => (vec![12], vec![-1]),
        2 => (vec![7, 16], vec![4, 12]),
        _ => (vec![3, 12, 21], vec![2, 8, 14]),
    }
}

/// Place every word of a phrase. Lines are a staircase, as on the stock app.
pub fn layout(p: &Phrase) -> Vec<Placed> {
    let (ys, xs) = line_geometry(p.lines.len());
    let mut out = Vec::new();
    for (i, line) in p.lines.iter().enumerate() {
        let mut x = if xs[i] < 0 {
            // Centred (the one-word NOON / MIDNIGHT case).
            ((W as i32 - font::text_width(line) as i32) / 2).max(0)
        } else {
            xs[i]
        };
        for w in line.split(' ') {
            out.push(Placed {
                word: w,
                line: i,
                x,
                y: ys[i],
            });
            x += font::word_width(w) as i32 + font::WORD_GAP as i32;
        }
    }
    out
}

/// Width in pixels of the widest line of a phrase, including its indent.
pub fn phrase_extent(p: &Phrase) -> i32 {
    layout(p)
        .iter()
        .map(|w| w.x + font::word_width(w.word) as i32)
        .max()
        .unwrap_or(0)
}

/// Seconds since midnight of the moment the phrase last changed, and how long
/// ago that was. The phrase turns over at the rounding boundary (:02:30,
/// :07:30, ...), so the whole animation is derived from the clock.
fn since_change(now: &NaiveDateTime) -> (f32, NaiveDateTime) {
    // `phrase` rounds whole minutes, so the phrase turns over on a minute
    // boundary - the minute where (m + 2) % 5 == 0, which is :03, :08, :13 and
    // so on. Deriving the boundary any other way (the obvious :02:30 rounding
    // instant, say) puts the animation half a slot away from the words it is
    // animating, which is exactly what the first version did.
    let secs = now.num_seconds_from_midnight() as i64;
    let min = secs.div_euclid(60);
    let boundary_min = (min + 2).div_euclid(5) * 5 - 2;
    let sub = now.and_utc().timestamp_subsec_millis() as f32 / 1000.0;
    let age = (secs - boundary_min * 60) as f32 + sub;
    // The phrase before this one: one minute before the boundary.
    let prev = *now - TimeDelta::try_seconds(age as i64 + 60).unwrap();
    (age, prev)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transition {
    /// Old word rolls up out of its line, new word rolls up into it.
    Roll,
    /// Old word slides out to the left, new word arrives from the right.
    Slide,
}

pub struct WordClock {
    /// Wall-clock time at `t = 0`.
    pub base: NaiveDateTime,
    pub panel: Panel,
    pub transition: Transition,
    /// Hue of the text, in turns (0.62 is the stock app's blue-white).
    pub hue: f32,
    /// Show the five-minute progress rule.
    pub progress: bool,
    cov: Vec<f32>,
    accent: Vec<f32>,
}

impl WordClock {
    pub fn at(base: NaiveDateTime) -> Self {
        WordClock {
            base,
            panel: NOMINAL,
            transition: Transition::Roll,
            hue: 0.62,
            progress: true,
            cov: vec![0.0; NPIX],
            accent: vec![0.0; NPIX],
        }
    }

    pub fn now() -> Self {
        Self::at(Local::now().naive_local())
    }

    fn time_at(&self, t: Duration) -> NaiveDateTime {
        self.base + TimeDelta::from_std(t).unwrap_or(TimeDelta::zero())
    }

    /// The text ramp for this moment: a 7-step linear-light ramp of one hue,
    /// every entry snapped to what the panel can emit. The hue drifts through
    /// the day - cool at night, a touch warmer in the afternoon - which is
    /// most of the "life" between phrase changes.
    fn text_ramp(&self, now: &NaiveDateTime) -> Vec<[u8; 3]> {
        let day = now.num_seconds_from_midnight() as f32 / 86400.0;
        let hue = self.hue + 0.055 * (day * std::f32::consts::TAU - 1.9).sin();
        let full = oklch_to_lin(0.88, 0.115, hue);
        (1..=TEXT_LEVELS)
            .map(|k| {
                let a = k as f32 / TEXT_LEVELS as f32;
                self.panel
                    .snap(lin_to_srgb8_3([full[0] * a, full[1] * a, full[2] * a]))
            })
            .collect()
    }

    fn accent_ramp(&self) -> Vec<[u8; 3]> {
        let full = oklch_to_lin(0.62, 0.11, self.hue - 0.10);
        (1..=ACCENT_LEVELS)
            .map(|k| {
                let a = k as f32 / ACCENT_LEVELS as f32;
                self.panel
                    .snap(lin_to_srgb8_3([full[0] * a, full[1] * a, full[2] * a]))
            })
            .collect()
    }

    /// Draw one word into the coverage buffer at a fractional position,
    /// clipped to its line band. Fractional offsets are split between two
    /// rows/columns - an exact box filter for a translation, which is what
    /// makes sub-pixel motion possible at this size without supersampling the
    /// whole frame.
    fn draw_word(&mut self, w: &Placed, dx: f32, dy: f32, gain: f32, band: (i32, i32)) {
        let x0 = w.x as f32 + dx;
        let y0 = w.y as f32 + dy;
        let cov = &mut self.cov;
        font::draw(w.word, 0, 0, |gx, gy| {
            let fx = x0 + gx as f32;
            let fy = y0 + gy as f32;
            let ix = fx.floor() as i32;
            let iy = fy.floor() as i32;
            let tx = fx - ix as f32;
            let ty = fy - iy as f32;
            for (oy, wy) in [(0, 1.0 - ty), (1, ty)] {
                if wy <= 0.0 {
                    continue;
                }
                let py = iy + oy;
                if py < band.0 || py >= band.1 || py < 0 || py >= H as i32 {
                    continue;
                }
                for (ox, wx) in [(0, 1.0 - tx), (1, tx)] {
                    if wx <= 0.0 {
                        continue;
                    }
                    let px = ix + ox;
                    if px < 0 || px >= W as i32 {
                        continue;
                    }
                    let i = py as usize * W + px as usize;
                    cov[i] = (cov[i] + wy * wx * gain).min(1.0);
                }
            }
        });
    }

    /// The moving sheen: a wide soft band of extra brightness crossing the
    /// panel every ~21 s. It reads as light moving over the letters and costs
    /// no extra colours, because it modulates within the same 7-step ramp.
    fn sheen(&self, x: usize, secs: f32) -> f32 {
        let pos = ((secs / 21.0).fract() * (W as f32 + 60.0)) - 30.0;
        let d = (x as f32 - pos) / 13.0;
        0.70 + 0.30 * (-d * d).exp()
    }

    /// The five-minute rule: a dim rail across the bottom row with a bright
    /// marker travelling along it, arriving at the right-hand end exactly as
    /// the words turn over.
    ///
    /// It started as a bar that filled, which at full width was the loudest
    /// thing on the panel and fought the type. A rail and a marker carry the
    /// same information at a third of the light. The marker moves 0.21 px/s,
    /// so it has to be drawn at sub-pixel positions or it would visibly jump
    /// once a second (brief section 3).
    fn draw_progress(&mut self, age: f32) {
        if !self.progress {
            return;
        }
        let p = (age / 300.0).clamp(0.0, 1.0);
        let pos = p * (W - 1) as f32;
        let row = 30 * W;
        for x in 0..W {
            self.accent[row + x] = 1.0 / ACCENT_LEVELS as f32;
        }
        // A soft 2 px marker, spread across the pixels it straddles.
        for x in 0..W {
            let d = (x as f32 - pos).abs();
            let v = (1.5 - d).clamp(0.0, 1.0);
            if v > 0.0 {
                self.accent[row + x] = self.accent[row + x].max(v);
            }
        }
    }

    fn compose(&mut self, t: Duration, out: &mut Indexed) {
        let now = self.time_at(t);
        let secs = t.as_secs_f32();
        let (age, prev) = since_change(&now);
        let cur = phrase(now.hour(), now.minute());
        let old = phrase(prev.hour(), prev.minute());
        self.cov.iter_mut().for_each(|v| *v = 0.0);
        self.accent.iter_mut().for_each(|v| *v = 0.0);

        let new_words = layout(&cur);
        let old_words = layout(&old);
        for w in &new_words {
            let band = self.band(w);
            if old_words.contains(w) {
                // This word did not change: it does not move. Only the words
                // that change should move.
                self.draw_word(w, 0.0, 0.0, 1.0, band);
                continue;
            }
            let p = self.roll_in(age, w.line);
            if p >= 1.0 {
                self.draw_word(w, 0.0, 0.0, 1.0, band);
            } else {
                let (dx, dy) = self.offset(1.0 - ease(p));
                self.draw_word(w, dx, dy, 1.0, band);
            }
        }
        for w in &old_words {
            if new_words.contains(w) {
                continue;
            }
            let p = self.roll_out(age, w.line);
            if p < 1.0 {
                let band = self.band(w);
                let (dx, dy) = self.offset(-ease(p));
                self.draw_word(w, dx, dy, 1.0, band);
            }
        }
        self.draw_progress(age);

        // Quantise into the palette.
        let text = self.text_ramp(&now);
        let accent = self.accent_ramp();
        out.palette.clear();
        out.palette.push([0, 0, 0]);
        out.palette.extend_from_slice(&text);
        out.palette.extend_from_slice(&accent);
        for y in 0..H {
            for x in 0..W {
                let i = y * W + x;
                let c = self.cov[i];
                let idx = if c > 0.0 {
                    let v = c * self.sheen(x, secs);
                    let lv = (v * TEXT_LEVELS as f32).round() as usize;
                    lv.min(TEXT_LEVELS)
                } else if self.accent[i] > 0.0 {
                    let lv = (self.accent[i] * ACCENT_LEVELS as f32).round() as usize;
                    if lv == 0 {
                        0
                    } else {
                        TEXT_LEVELS + lv.min(ACCENT_LEVELS)
                    }
                } else {
                    0
                };
                out.indices[i] = idx as u8;
            }
        }
    }

    /// The vertical band a word's line occupies; words are clipped to it so a
    /// rolling word never collides with its neighbours.
    fn band(&self, w: &Placed) -> (i32, i32) {
        (w.y, w.y + ROWS as i32)
    }

    /// How far through its exit the outgoing word is, 0..=1.
    ///
    /// Exit and entry are *sequenced*, not simultaneous: the old word is
    /// clear of the band before the new one starts into it. Running both at
    /// once - the obvious way, and the way this first worked - puts two words
    /// in the same seven rows for a third of a second, and what you see is not
    /// a transition but a smear of broken letters.
    fn roll_out(&self, age: f32, line: usize) -> f32 {
        let start = LINE_STAGGER * line as f32;
        ((age - start) / (ROLL_SECS * 0.45)).clamp(0.0, 1.0)
    }

    /// How far through its entrance the incoming word is, 0..=1.
    fn roll_in(&self, age: f32, line: usize) -> f32 {
        let start = LINE_STAGGER * line as f32 + ROLL_SECS * 0.55;
        ((age - start) / (ROLL_SECS * 0.45)).clamp(0.0, 1.0)
    }

    /// Where a word sits at roll progress `p`: p = 0 is home, p = -1 has it
    /// gone upward (leaving), p = 1 has it below (about to arrive).
    fn offset(&self, p: f32) -> (f32, f32) {
        match self.transition {
            Transition::Roll => (0.0, p * (ROWS as f32 + 1.0)),
            Transition::Slide => (p * -(W as f32 * 0.55), 0.0),
        }
    }
}

impl Piece for WordClock {
    fn name(&self) -> &'static str {
        "clock"
    }

    fn render(&mut self, t: Duration, out: &mut Frame) {
        let mut idx = Indexed::default();
        self.compose(t, &mut idx);
        *out = idx.to_frame();
    }

    fn render_indexed(&mut self, t: Duration, out: &mut Indexed) -> bool {
        self.compose(t, out);
        true
    }
}

/// Every five-minute slot of a day, as (hour, minute) of the *unrounded* time
/// at the middle of the slot. Used by the tests.
/// Ease in-out, so words leave and arrive without a mechanical constant
/// velocity.
fn ease(p: f32) -> f32 {
    p * p * (3.0 - 2.0 * p)
}

pub fn all_slots() -> impl Iterator<Item = (u32, u32)> {
    (0..288).map(|i| {
        let t = i * 5;
        (t / 60, t % 60)
    })
}
