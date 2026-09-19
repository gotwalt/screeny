//! Palette design: median cut in Oklab, then weighted Lloyd refinement.
//!
//! Lifted from `lab/src/enc/quant.rs` (card 002). Oklab rather than sRGB
//! because the whole point of a small palette is to put its few entries where
//! the eye will miss them least.
//!
//! Four changes for card 031, all of them speed:
//!
//! * the histogram is passed in, not rebuilt (see [`super::hist`]);
//! * the previous frame's palette is **always** offered as the Lloyd seed, not
//!   only in the temporal-palette mode, and a seeded start converges in fewer
//!   iterations;
//! * Lloyd stops early once the centroids stop moving, which on flat content
//!   is after two or three of the ten iterations the lab always ran;
//! * every "which entry is nearest" question goes through a k-d tree
//!   ([`super::nn`]) instead of a linear scan over the palette. That one
//!   change is most of the speedup and costs nothing, because it returns the
//!   same answer.
//!
//! The safety valve from the lab is kept - a fresh median cut wins if the
//! seeded palette is more than 15% worse, so a scene cut cannot leave a stale
//! palette latched - but it is no longer computed every frame; see
//! [`SEED_DRIFT`].
//!
//! Seeding turns out to be worth quality as well as time: measured against
//! the frozen lab on its own five clips, seeded palettes came out *ahead* on
//! mean panel-aware dE (6.08 against 6.14) because a palette that tracks the
//! previous frame's flickers less. The exception is a fast zoom, where the
//! seed is genuinely stale; that is what [`RESEED_EVERY`] bounds.

use screeny_panel::color::{d2, lin, lin_to_srgb8, oklab_inv, oklab_srgb8};

use super::hist::Hist;
use super::nn::NnIndex;

/// A palette in the three representations the encoders need.
#[derive(Clone, Debug, Default)]
pub struct Palette {
    /// What goes on the wire.
    pub srgb: Vec<[u8; 3]>,
    /// Oklab, for "nearest entry" decisions.
    pub lab: Vec<[f32; 3]>,
    /// Linear light, for solving dither mix ratios.
    pub lin: Vec<[f32; 3]>,
}

impl Palette {
    /// Derive the other two representations from the wire colours.
    #[must_use]
    pub fn from_srgb(srgb: Vec<[u8; 3]>) -> Self {
        let lab = srgb.iter().map(|c| oklab_srgb8(*c)).collect();
        let lin = srgb.iter().map(|c| lin(*c)).collect();
        Palette { srgb, lab, lin }
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.srgb.len()
    }

    /// True if the palette has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.srgb.is_empty()
    }

    /// Index of the nearest entry to an Oklab colour.
    #[inline]
    #[must_use]
    pub fn nearest(&self, lab: [f32; 3]) -> usize {
        let mut bi = 0;
        let mut bd = f32::MAX;
        for (i, c) in self.lab.iter().enumerate() {
            let d = d2(lab, *c);
            if d < bd {
                bd = d;
                bi = i;
            }
        }
        bi
    }
}

/// How hard the quantiser works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuantEffort {
    /// Maximum Lloyd iterations from a fresh median cut.
    pub iters: usize,
    /// Maximum Lloyd iterations from a seeded start.
    pub seeded_iters: usize,
    /// Whether a seeded palette is ever checked against a fresh median cut.
    pub verify_seed: bool,
}

impl QuantEffort {
    /// The lab's settings: ten iterations at k<=32, and a fresh median cut
    /// kept as the safety valve. The baseline card 031 measures against.
    pub const FULL: QuantEffort = QuantEffort {
        iters: 10,
        seeded_iters: 10,
        verify_seed: true,
    };
    /// Fewer iterations, seeded starts trusted without a second opinion.
    pub const FAST: QuantEffort = QuantEffort {
        iters: 4,
        seeded_iters: 2,
        verify_seed: false,
    };
}

/// A palette and what it costs on the frame it was built for.
pub struct Built {
    /// The palette.
    pub palette: Palette,
    /// Total weighted squared Oklab error over the histogram.
    pub cost: f64,
}

/// A seeded palette is accepted without a second opinion while its cost stays
/// within this factor of the cost the same palette size had on the previous
/// frame. Beyond it, the content has changed enough that a stale palette might
/// be latched, and a fresh median cut is computed and compared.
const SEED_DRIFT: f64 = 1.25;

/// ...and regardless of drift, a fresh median cut is computed this often, so
/// a slow scene change cannot creep past the drift test one frame at a time.
/// Once a second at 30 fps.
const RESEED_EVERY: usize = 30;

/// Iterations actually worth spending at this palette size. Large palettes
/// are already close after median cut.
fn iters_for(k: usize, base: usize) -> usize {
    if k <= 32 {
        base
    } else if k <= 64 {
        base.div_ceil(2).max(1)
    } else {
        base.div_ceil(3).max(1)
    }
}

/// Build a `k`-entry palette for the frame `hist` describes.
///
/// `seed`, when it has exactly `k` entries, is the previous frame's palette
/// and is used as the Lloyd starting point: it converges in fewer iterations
/// *and* keeps palettes similar frame to frame, which is worth a visible
/// amount of flicker (card 002).
///
/// The lab guarded against a stale palette latching across a scene cut by
/// computing a fresh median cut every frame and keeping the seeded one only
/// if it was within 15%. That guard is kept but is no longer paid for every
/// frame: `prev_cost` is what this palette size cost on the previous frame,
/// and while the seeded palette stays within [`SEED_DRIFT`] of it nothing has
/// changed enough to be worth a second opinion. Every [`RESEED_EVERY`] frames
/// the fresh build happens anyway, so a slow change cannot creep past.
///
/// Requires [`Hist::ensure_lab`].
#[must_use]
pub fn build(
    hist: &Hist,
    k: usize,
    seed: Option<&[[u8; 3]]>,
    prev_cost: Option<f64>,
    frame_idx: usize,
    effort: QuantEffort,
) -> Built {
    if hist.len() <= k {
        let mut srgb: Vec<[u8; 3]> = hist.bins.iter().map(|b| b.srgb).collect();
        while srgb.len() < k {
            srgb.push([0, 0, 0]);
        }
        return Built {
            palette: Palette::from_srgb(srgb),
            cost: 0.0,
        };
    }

    let fresh = |hist: &Hist| {
        let c = lloyd(hist, median_cut(hist, k), iters_for(k, effort.iters));
        let cost = hist.palette_cost(&NnIndex::build(&c));
        (c, cost)
    };

    let (chosen, cost) = match seed {
        Some(s) if s.len() == k => {
            let start: Vec<[f32; 3]> = s.iter().map(|c| oklab_srgb8(*c)).collect();
            let seeded = lloyd(hist, start, iters_for(k, effort.seeded_iters));
            let cs = hist.palette_cost(&NnIndex::build(&seeded));
            let drifted = prev_cost.is_none_or(|p| cs > p * SEED_DRIFT);
            let due = frame_idx.is_multiple_of(RESEED_EVERY);
            if effort.verify_seed && (drifted || due) {
                let (f, cf) = fresh(hist);
                if cs <= cf * 1.15 {
                    (seeded, cs)
                } else {
                    (f, cf)
                }
            } else {
                (seeded, cs)
            }
        }
        _ => fresh(hist),
    };
    Built {
        palette: Palette::from_srgb(chosen.iter().map(|c| lab_to_srgb8(*c)).collect()),
        cost,
    }
}

fn lab_to_srgb8(lab: [f32; 3]) -> [u8; 3] {
    let l = oklab_inv(lab);
    [lin_to_srgb8(l[0]), lin_to_srgb8(l[1]), lin_to_srgb8(l[2])]
}

/// One box of the median cut, with its split score cached.
struct Box {
    members: Vec<u32>,
    axis: usize,
    /// `span * sqrt(weight)`, or 0 for a box that cannot be split.
    score: f32,
}

/// Median cut over Oklab boxes, splitting the box with the largest
/// count-weighted spread along its longest axis.
///
/// The lab recomputed every box's longest axis on every one of the `k - 1`
/// splits, which is `O(k * bins)` - a quarter of a million axis evaluations
/// for a photographic frame at `k = 256`, and the second biggest cost card
/// 031 found. A split only changes the two boxes it creates, so the score is
/// cached and recomputed for those two alone.
fn median_cut(hist: &Hist, k: usize) -> Vec<[f32; 3]> {
    let lab = hist.lab();
    let mut boxes: Vec<Box> = vec![score_box(hist, (0..hist.len() as u32).collect())];
    while boxes.len() < k {
        let mut best = None;
        let mut best_score = 0f32;
        for (i, b) in boxes.iter().enumerate() {
            if b.score > best_score {
                best_score = b.score;
                best = Some(i);
            }
        }
        let Some(i) = best else { break };
        let axis = boxes[i].axis;
        let mut b = boxes.swap_remove(i).members;
        b.sort_by(|&p, &q| {
            lab[p as usize][axis]
                .partial_cmp(&lab[q as usize][axis])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let total: f32 = b.iter().map(|&j| hist.bins[j as usize].count).sum();
        let mut acc = 0.0;
        let mut cut = 1;
        for (j, &idx) in b.iter().enumerate() {
            acc += hist.bins[idx as usize].count;
            if acc >= total / 2.0 {
                cut = (j + 1).min(b.len() - 1).max(1);
                break;
            }
        }
        let right = b.split_off(cut);
        boxes.push(score_box(hist, b));
        boxes.push(score_box(hist, right));
    }
    boxes.iter().map(|b| centroid(hist, &b.members)).collect()
}

fn score_box(hist: &Hist, members: Vec<u32>) -> Box {
    if members.len() < 2 {
        return Box {
            members,
            axis: 0,
            score: 0.0,
        };
    }
    let (axis, span) = longest_axis(hist, &members);
    let n: f32 = members.iter().map(|&j| hist.bins[j as usize].count).sum();
    Box {
        members,
        axis,
        score: span * n.sqrt(),
    }
}

fn centroid(hist: &Hist, idxs: &[u32]) -> [f32; 3] {
    let lab = hist.lab();
    let mut acc = [0f64; 3];
    let mut w = 0f64;
    for &i in idxs {
        let n = hist.bins[i as usize].count as f64;
        for k in 0..3 {
            acc[k] += lab[i as usize][k] as f64 * n;
        }
        w += n;
    }
    if w == 0.0 {
        return [0.0; 3];
    }
    [
        (acc[0] / w) as f32,
        (acc[1] / w) as f32,
        (acc[2] / w) as f32,
    ]
}

fn longest_axis(hist: &Hist, idxs: &[u32]) -> (usize, f32) {
    let lab = hist.lab();
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for &i in idxs {
        for k in 0..3 {
            let v = lab[i as usize][k];
            lo[k] = lo[k].min(v);
            hi[k] = hi[k].max(v);
        }
    }
    let mut best = 0;
    let mut span = -1.0;
    for k in 0..3 {
        let s = hi[k] - lo[k];
        if s > span {
            span = s;
            best = k;
        }
    }
    (best, span)
}

/// Weighted k-means over the histogram bins, stopping early once no centroid
/// moves by more than `EPS`.
fn lloyd(hist: &Hist, mut cents: Vec<[f32; 3]>, iters: usize) -> Vec<[f32; 3]> {
    /// Oklab L spans 0..1, so this is a movement of one part in 100 000:
    /// far below the 1/255 code step the palette is rounded to anyway.
    const EPS: f32 = 1e-5;

    let k = cents.len();
    let lab = hist.lab();
    let mut acc = vec![[0f64; 3]; k];
    let mut wsum = vec![0f64; k];
    for _ in 0..iters {
        acc.fill([0f64; 3]);
        wsum.fill(0.0);
        let index = NnIndex::build(&cents);
        for (i, b) in hist.bins.iter().enumerate() {
            let l = lab[i];
            let bi = index.nearest(l).0;
            let n = b.count as f64;
            for j in 0..3 {
                acc[bi][j] += l[j] as f64 * n;
            }
            wsum[bi] += n;
        }
        let mut moved = 0f32;
        for i in 0..k {
            if wsum[i] > 0.0 {
                let next = [
                    (acc[i][0] / wsum[i]) as f32,
                    (acc[i][1] / wsum[i]) as f32,
                    (acc[i][2] / wsum[i]) as f32,
                ];
                moved = moved.max(d2(next, cents[i]));
                cents[i] = next;
            }
        }
        if moved <= EPS * EPS {
            break;
        }
    }
    cents
}

/// Nearest palette entry for every histogram bin. This is the whole of an
/// undithered palette mapping: the per-pixel plane is then one array read per
/// pixel through [`Hist::bin_of`].
///
/// Requires [`Hist::ensure_lab`].
#[must_use]
pub fn nearest_per_bin(hist: &Hist, pal: &Palette) -> Vec<u8> {
    let index = NnIndex::build(&pal.lab);
    hist.lab()
        .iter()
        .map(|l| index.nearest(*l).0 as u8)
        .collect()
}
