//! Captured finger motion: takes recorded from a webcam with
//! `tools/handcap` (MediaPipe's hand landmarker, 21 joints a frame in
//! millimetres about the hand's centre), read once, filled where the tracker
//! lost the hand, resampled to an even rate and smoothed of its jitter.
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
/// The smoothing, a gaussian this many frames wide (one sigma): enough to
/// still the tracker's millimetre jitter, not so much that a finger's flick
/// goes soft.
const SIGMA: f32 = 1.6;

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
