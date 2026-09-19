//! Exact nearest-neighbour lookup over a palette, in Oklab.
//!
//! This is card 031's single biggest win. Everything the quantiser does is
//! "which of these `k` colours is this colour closest to", asked once per
//! distinct colour per Lloyd iteration and again for every mapping and every
//! cost evaluation. Done by linear scan that is `O(bins * k)`: at `k = 256`
//! and 2048 distinct colours it is half a million distance evaluations per
//! iteration, and it was 85% of the encoder's wall clock.
//!
//! A three-dimensional k-d tree answers the same question in `O(log k)` on
//! average. **It answers it identically**: the leaf scan and the pruning test
//! are arranged so that ties go to the lowest index, exactly as the linear
//! scan's strict `<` does, so swapping this in cannot change a single output
//! byte - only the time taken to produce it.

use screeny_panel::color::d2;

/// Points per leaf. Small enough that the tree prunes, large enough that the
/// leaf scan is a tight loop over contiguous memory.
const LEAF: usize = 8;

struct Node {
    /// Split axis, or `u8::MAX` for a leaf.
    axis: u8,
    split: f32,
    /// Range of `pts` this leaf covers (leaves only).
    lo: u32,
    hi: u32,
    left: u32,
    right: u32,
}

/// A k-d tree over up to 256 palette colours.
pub struct NnIndex {
    pts: Vec<([f32; 3], u32)>,
    nodes: Vec<Node>,
}

impl NnIndex {
    /// Build an index over `pts`, which are indexed by position.
    #[must_use]
    pub fn build(pts: &[[f32; 3]]) -> Self {
        let mut idx = NnIndex {
            pts: pts
                .iter()
                .enumerate()
                .map(|(i, p)| (*p, i as u32))
                .collect(),
            nodes: Vec::with_capacity(pts.len() / LEAF * 2 + 2),
        };
        let n = idx.pts.len();
        idx.build_range(0, n);
        idx
    }

    fn build_range(&mut self, lo: usize, hi: usize) -> u32 {
        let id = self.nodes.len() as u32;
        if hi - lo <= LEAF {
            self.nodes.push(Node {
                axis: u8::MAX,
                split: 0.0,
                lo: lo as u32,
                hi: hi as u32,
                left: u32::MAX,
                right: u32::MAX,
            });
            return id;
        }
        // Split the widest axis at the median, which keeps the tree balanced
        // whatever shape the palette has in Oklab.
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for (p, _) in &self.pts[lo..hi] {
            for k in 0..3 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        let mut axis = 0usize;
        for k in 1..3 {
            if max[k] - min[k] > max[axis] - min[axis] {
                axis = k;
            }
        }
        let mid = usize::midpoint(lo, hi);
        self.pts[lo..hi].select_nth_unstable_by(mid - lo, |a, b| {
            a.0[axis]
                .partial_cmp(&b.0[axis])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let split = self.pts[mid].0[axis];
        self.nodes.push(Node {
            axis: axis as u8,
            split,
            lo: 0,
            hi: 0,
            left: u32::MAX,
            right: u32::MAX,
        });
        let left = self.build_range(lo, mid);
        let right = self.build_range(mid, hi);
        self.nodes[id as usize].left = left;
        self.nodes[id as usize].right = right;
        id
    }

    /// The index of the nearest point to `q`, and its squared distance.
    ///
    /// Ties go to the lowest index, matching a linear scan with a strict `<`.
    #[must_use]
    pub fn nearest(&self, q: [f32; 3]) -> (usize, f32) {
        let mut best = (usize::MAX, f32::MAX);
        self.search(0, q, &mut best);
        best
    }

    fn search(&self, id: u32, q: [f32; 3], best: &mut (usize, f32)) {
        let n = &self.nodes[id as usize];
        if n.axis == u8::MAX {
            for (p, i) in &self.pts[n.lo as usize..n.hi as usize] {
                let d = d2(q, *p);
                let i = *i as usize;
                // Exact equality is the point: this reproduces a linear
                // scan's tie-break, and an epsilon would not.
                #[allow(clippy::float_cmp)]
                let tie = d == best.1 && i < best.0;
                if d < best.1 || tie {
                    *best = (i, d);
                }
            }
            return;
        }
        let delta = q[n.axis as usize] - n.split;
        let (near, far) = if delta < 0.0 {
            (n.left, n.right)
        } else {
            (n.right, n.left)
        };
        self.search(near, q, best);
        // `<=` rather than `<`: an equally distant point on the far side may
        // have a lower index, and that is what keeps this identical to the
        // linear scan it replaced.
        if delta * delta <= best.1 {
            self.search(far, q, best);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(pts: &[[f32; 3]], q: [f32; 3]) -> (usize, f32) {
        let mut best = (usize::MAX, f32::MAX);
        for (i, p) in pts.iter().enumerate() {
            let d = d2(q, *p);
            if d < best.1 {
                best = (i, d);
            }
        }
        best
    }

    /// The whole point of the index is that it changes nothing, including the
    /// tie-break, so this is checked against a linear scan on random data and
    /// on data full of deliberate duplicates.
    #[test]
    fn agrees_with_linear_scan() {
        let mut s = 0x2545_f491u32;
        let mut rnd = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as f32 / 16_777_216.0
        };
        for k in [1usize, 2, 3, 7, 8, 9, 16, 31, 32, 64, 127, 256] {
            for dup in [false, true] {
                let pts: Vec<[f32; 3]> = (0..k)
                    .map(|i| {
                        if dup && i % 3 == 0 {
                            [0.25, 0.5, 0.75]
                        } else {
                            [rnd(), rnd() - 0.5, rnd() - 0.5]
                        }
                    })
                    .collect();
                let idx = NnIndex::build(&pts);
                for _ in 0..200 {
                    let q = [rnd(), rnd() - 0.5, rnd() - 0.5];
                    assert_eq!(idx.nearest(q), linear(&pts, q), "k={k} dup={dup}");
                }
                // Every point must find itself.
                for (i, p) in pts.iter().enumerate() {
                    let (j, d) = idx.nearest(*p);
                    assert!(d == 0.0, "a point did not find itself: d={d}");
                    assert!(j <= i, "k={k} dup={dup}: {j} > {i}");
                }
            }
        }
    }
}
