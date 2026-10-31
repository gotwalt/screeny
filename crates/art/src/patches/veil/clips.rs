//! Captured finger motion: takes recorded from a webcam with
//! `tools/handcap` (MediaPipe's hand landmarker, 21 joints a frame in
//! millimetres about the hand's centre), read once and cleaned: turned to sit
//! on the palm, rid of the frames where the tracker glitched a finger
//! somewhere it never was, filled where it lost the hand, resampled to an
//! even rate and smoothed of its jitter.
//! `hand.rs` turns a frame into the bend of each of the model's knuckles.

use serde_json::Value;
use std::sync::OnceLock;

type V3 = [f32; 3];

/// The takes, in the order veil's `take` setting counts them from 1. A take
/// of a left hand is mirrored into a right one.
const TAKES: &[(&str, &str)] = &[
    ("cycle", include_str!("clips/cycle-01.json")),
    ("flatspread", include_str!("clips/flatspread-01.json")),
    ("flop", include_str!("clips/flop-01.json")),
    ("peace", include_str!("clips/peace-01.json")),
    ("point", include_str!("clips/point-01.json")),
    ("pointing", include_str!("clips/pointing-01.json")),
    ("pointing 2", include_str!("clips/pointing-02.json")),
    ("reach", include_str!("clips/reach-01.json")),
    ("slowmo", include_str!("clips/slowmo-01.json")),
    ("thumbs up", include_str!("clips/thumbs-up-01.json")),
    ("wave", include_str!("clips/wave-01.json")),
];

pub const COUNT: usize = TAKES.len();

/// Frames a second, after resampling.
const RATE: f32 = 30.0;
/// The smoothing, a gaussian this many frames wide (one sigma, 130 ms):
/// the tracker's millimetre jitter, played slowly, reads as a tremble, so
/// only the hand's deliberate motion is kept.
const SIGMA: f32 = 4.0;

/// A glitch: a joint further than this (mm) from where it was across the
/// frames around it - the median of `GLITCH_SPAN` either side - is the
/// tracker's mistake, not the hand's motion, and is filled in from the
/// frames that agree. Real fingers sweep smoothly; a glitch jumps there and
/// back, or jumps and stays only a few frames.
const GLITCH_MM: f32 = 9.0;
const GLITCH_SPAN: usize = 5;

pub struct Clip {
    frames: Vec<[V3; 21]>,
}

pub fn all() -> &'static [Clip] {
    static CLIPS: OnceLock<Vec<Clip>> = OnceLock::new();
    CLIPS.get_or_init(|| TAKES.iter().map(|&(name, json)| Clip::read(name, json)).collect())
}

impl Clip {
    fn read(name: &'static str, json: &str) -> Clip {
        let v: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("clip {name}: {e}"));
        assert_eq!(v["format"], "screeny-hand-clip", "clip {name}");
        // (ms, joints) for every frame the tracker saw the hand.
        let seen: Vec<(f32, [V3; 21])> = v["frames"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f.get("lost").is_none())
            .map(|f| {
                let w: Vec<f32> = f["world"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
                (f["t"].as_f64().unwrap() as f32, std::array::from_fn(|j| [w[3 * j], w[3 * j + 1], w[3 * j + 2]]))
            })
            .collect();
        assert!(seen.len() > 10, "clip {name}: the hand is barely in it");
        let mut seen: Vec<(f32, [V3; 21])> = seen.into_iter().map(|(t, p)| (t, on_palm(&p))).collect();
        for _ in 0..2 {
            unglitch(&mut seen);
        }

        // An even rate, straight across where the hand was lost.
        let (first, last) = (seen[0].0, seen[seen.len() - 1].0);
        let n = ((last - first) / 1000.0 * RATE) as usize + 1;
        let mut at = 0;
        let mut frames: Vec<[V3; 21]> = (0..n)
            .map(|i| {
                let t = first + i as f32 * 1000.0 / RATE;
                while at + 2 < seen.len() && seen[at + 1].0 <= t {
                    at += 1;
                }
                let ((ta, a), (tb, b)) = (&seen[at], &seen[(at + 1).min(seen.len() - 1)]);
                let u = if tb > ta { ((t - ta) / (tb - ta)).clamp(0.0, 1.0) } else { 0.0 };
                std::array::from_fn(|j| std::array::from_fn(|k| a[j][k] + (b[j][k] - a[j][k]) * u))
            })
            .collect();

        let r = (3.0 * SIGMA).ceil() as isize;
        let kernel: Vec<f32> = (-r..=r).map(|k| (-(k * k) as f32 / (2.0 * SIGMA * SIGMA)).exp()).collect();
        let raw = frames.clone();
        for (i, f) in frames.iter_mut().enumerate() {
            let mut sum = [[0.0; 3]; 21];
            let mut total = 0.0;
            for (k, w) in kernel.iter().enumerate() {
                let src = (i as isize + k as isize - r).clamp(0, n as isize - 1) as usize;
                for j in 0..21 {
                    for c in 0..3 {
                        sum[j][c] += raw[src][j][c] * w;
                    }
                }
                total += w;
            }
            *f = sum.map(|p| p.map(|x| x / total));
        }

        if !right_handed(&frames) {
            for f in &mut frames {
                for p in f.iter_mut() {
                    p[0] = -p[0];
                }
            }
        }
        Clip { frames }
    }

    /// How long the take runs, in seconds.
    pub fn seconds(&self) -> f32 {
        (self.frames.len() - 1) as f32 / RATE
    }

    /// The joints at `t` seconds, played forward then back again, so it
    /// never jumps.
    pub fn at(&self, t: f32) -> [V3; 21] {
        let len = self.seconds();
        let mut u = t.rem_euclid(2.0 * len);
        if u > len {
            u = 2.0 * len - u;
        }
        let x = u * RATE;
        let i = (x as usize).min(self.frames.len() - 2);
        let k = x - i as f32;
        let (a, b) = (&self.frames[i], &self.frames[i + 1]);
        std::array::from_fn(|j| std::array::from_fn(|c| a[j][c] + (b[j][c] - a[j][c]) * k))
    }
}

/// The joints as the palm holds them: from the wrist, along the middle
/// knuckle, across the knuckles and out of the palm. Only how the fingers
/// move against the palm is kept, so a turn of the whole hand cannot look
/// like a finger moving, and a glitch is found in one finger alone.
fn on_palm(p: &[V3; 21]) -> [V3; 21] {
    let sub = |a: V3, b: V3| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: V3, b: V3| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let unit = |a: V3| {
        let l = dot(a, a).sqrt().max(1e-6);
        [a[0] / l, a[1] / l, a[2] / l]
    };
    let along = unit(sub(p[9], p[0]));
    let across = sub(p[5], p[17]);
    let k = dot(along, across);
    let across = unit([across[0] - along[0] * k, across[1] - along[1] * k, across[2] - along[2] * k]);
    let out = [along[1] * across[2] - along[2] * across[1], along[2] * across[0] - along[0] * across[2], along[0] * across[1] - along[1] * across[0]];
    p.map(|q| {
        let d = sub(q, p[0]);
        [dot(d, along), dot(d, across), dot(d, out)]
    })
}

/// Finds each joint's glitches against the median of the frames around it,
/// and fills them in straight between the nearest frames on either side that
/// are not.
fn unglitch(seen: &mut [(f32, [V3; 21])]) {
    let n = seen.len();
    let median = |v: &mut Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    for j in 0..21 {
        let bad: Vec<bool> = (0..n)
            .map(|i| {
                let span = i.saturating_sub(GLITCH_SPAN)..(i + GLITCH_SPAN + 1).min(n);
                let m: [f32; 3] = std::array::from_fn(|c| median(&mut seen[span.clone()].iter().map(|f| f.1[j][c]).collect()));
                let d = seen[i].1[j];
                ((d[0] - m[0]).powi(2) + (d[1] - m[1]).powi(2) + (d[2] - m[2]).powi(2)).sqrt() > GLITCH_MM
            })
            .collect();
        let mut i = 0;
        while i < n {
            if !bad[i] {
                i += 1;
                continue;
            }
            let end = (i..n).find(|&k| !bad[k]).unwrap_or(n);
            let (before, after) = (i.checked_sub(1), (end < n).then_some(end));
            for k in i..end {
                seen[k].1[j] = match (before, after) {
                    (Some(a), Some(b)) => {
                        let u = (seen[k].0 - seen[a].0) / (seen[b].0 - seen[a].0);
                        std::array::from_fn(|c| seen[a].1[j][c] + (seen[b].1[j][c] - seen[a].1[j][c]) * u)
                    }
                    (Some(a), None) => seen[a].1[j],
                    (None, Some(b)) => seen[b].1[j],
                    (None, None) => seen[k].1[j],
                };
            }
            i = end;
        }
    }
}

/// Whether the take is of a right hand: fingers curl towards the palm, and
/// on a right hand (MediaPipe's axes being right-handed) the palm is the
/// side `(index knuckle - wrist) x (little knuckle - wrist)` points to.
fn right_handed(frames: &[[V3; 21]]) -> bool {
    let sub = |a: V3, b: V3| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let mut votes = 0i64;
    for p in frames {
        let (a, b) = (sub(p[5], p[0]), sub(p[17], p[0]));
        let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        for (knuckle, tip) in [(9, 12), (13, 16)] {
            let d = sub(p[tip], p[knuckle]);
            votes += if n[0] * d[0] + n[1] * d[1] + n[2] * d[2] > 0.0 { 1 } else { -1 };
        }
    }
    votes > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_take_reads_as_a_right_hand() {
        for (c, (name, _)) in all().iter().zip(TAKES) {
            assert!(c.seconds() > 3.0, "{name}: {} s", c.seconds());
            assert!(right_handed(&c.frames), "{name}");
        }
    }

    /// Cleaned, no joint moves faster than a hand deliberately moves a
    /// finger, frame to frame, in any take.
    #[test]
    fn no_take_has_a_finger_jump() {
        for (c, (name, _)) in all().iter().zip(TAKES) {
            for (i, w) in c.frames.windows(2).enumerate() {
                let fastest = (0..21)
                    .map(|j| ((w[1][j][0] - w[0][j][0]).powi(2) + (w[1][j][1] - w[0][j][1]).powi(2) + (w[1][j][2] - w[0][j][2]).powi(2)).sqrt())
                    .fold(0.0, f32::max);
                assert!(fastest * RATE < 450.0, "{name}: a joint moves {:.0} mm/s at frame {i}", fastest * RATE);
            }
        }
    }

    #[test]
    fn playing_back_and_forth_never_jumps() {
        let c = &all()[0];
        let step = 1.0 / 30.0;
        for i in 0..(4.0 * c.seconds() / step) as usize {
            let (a, b) = (c.at(i as f32 * step), c.at((i + 1) as f32 * step));
            let moved = (0..21).map(|j| (0..3).map(|k| (a[j][k] - b[j][k]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max);
            assert!(moved < 25.0, "a {moved} mm jump at frame {i}");
        }
    }
}
