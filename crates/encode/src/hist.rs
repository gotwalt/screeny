//! The frame's colour histogram, built once and shared by everything.
//!
//! Card 031 found the lab rebuilding this up to six times per frame: once in
//! the ladder and once inside every `quant::build` call, each time through a
//! `HashMap<[u8; 3], f32>` with SipHash. Here it is built once per frame into
//! reusable storage, with an open-addressed table keyed on the packed colour,
//! and every consumer borrows it.
//!
//! Two further things fall out of building it here:
//!
//! * `bin_of[p]` - which bin each pixel belongs to. Computed for free while
//!   hashing, and it turns every palette mapping from "O(NPIX * k) distance
//!   evaluations" into "O(bins * k), then one array read per pixel".
//! * `lab` - the Oklab of each distinct colour, computed **lazily**. A frame
//!   that takes the lossless rung of the ladder never needs it.

use screeny_proto::{Rgb888Frame, NPIX};

use screeny_panel::color::{oklab_srgb8, pack, unpack};

/// One distinct colour of a frame.
#[derive(Clone, Copy, Debug)]
pub struct Bin {
    /// The colour.
    pub srgb: [u8; 3],
    /// How many pixels have it.
    pub count: f32,
}

/// A frame's distinct colours, in ascending packed-colour order.
///
/// Reusable: [`Hist::rebuild`] keeps the allocations from the previous frame.
pub struct Hist {
    /// Distinct colours, sorted by [`screeny_panel::color::pack`].
    pub bins: Vec<Bin>,
    /// Oklab of each bin, or empty until [`Hist::ensure_lab`] is called.
    lab: Vec<[f32; 3]>,
    /// Bin index of every pixel, raster order.
    bin_of: Vec<u32>,
    // Open-addressed colour -> bin index map, power-of-two sized.
    slot_key: Vec<u32>,
    slot_val: Vec<u32>,
    mask: usize,
}

impl Default for Hist {
    fn default() -> Self {
        Self::new()
    }
}

impl Hist {
    /// An empty histogram with room for a full frame of distinct colours.
    #[must_use]
    pub fn new() -> Self {
        // NPIX distinct colours at worst; 2x that in slots keeps the load
        // factor at 0.5 and the probe chains short.
        let n = (NPIX * 2).next_power_of_two();
        Hist {
            bins: Vec::with_capacity(256),
            lab: Vec::new(),
            bin_of: vec![0u32; NPIX],
            slot_key: vec![u32::MAX; n],
            slot_val: vec![0u32; n],
            mask: n - 1,
        }
    }

    #[inline(always)]
    fn slot(&self, key: u32) -> usize {
        ((key.wrapping_mul(2_654_435_761) >> 11) as usize) & self.mask
    }

    /// Recompute for `f`, reusing storage.
    pub fn rebuild(&mut self, f: &Rgb888Frame) {
        self.slot_key.fill(u32::MAX);
        self.bins.clear();
        self.lab.clear();

        // Pass 1: count, in first-seen order.
        let mut tmp: Vec<(u32, f32)> = Vec::with_capacity(256);
        for p in 0..NPIX {
            let key = pack([f[p * 3], f[p * 3 + 1], f[p * 3 + 2]]);
            let mut s = self.slot(key);
            loop {
                if self.slot_key[s] == key {
                    let i = self.slot_val[s] as usize;
                    tmp[i].1 += 1.0;
                    self.bin_of[p] = self.slot_val[s];
                    break;
                }
                if self.slot_key[s] == u32::MAX {
                    let i = tmp.len() as u32;
                    self.slot_key[s] = key;
                    self.slot_val[s] = i;
                    tmp.push((key, 1.0));
                    self.bin_of[p] = i;
                    break;
                }
                s = (s + 1) & self.mask;
            }
        }

        // Pass 2: sort into packed-colour order so runs are reproducible, and
        // rewrite both the map and `bin_of` to the sorted indices.
        let mut order: Vec<u32> = (0..tmp.len() as u32).collect();
        order.sort_unstable_by_key(|&i| tmp[i as usize].0);
        let mut new_of = vec![0u32; tmp.len()];
        self.bins.reserve(tmp.len());
        for (new, &old) in order.iter().enumerate() {
            new_of[old as usize] = new as u32;
            let (key, count) = tmp[old as usize];
            self.bins.push(Bin {
                srgb: unpack(key),
                count,
            });
        }
        for v in &mut self.bin_of {
            *v = new_of[*v as usize];
        }
        for s in 0..=self.mask {
            if self.slot_key[s] != u32::MAX {
                self.slot_val[s] = new_of[self.slot_val[s] as usize];
            }
        }
    }

    /// Number of distinct colours.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bins.len()
    }

    /// True if the frame had no pixels, which cannot happen for a real frame.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bins.is_empty()
    }

    /// Bin index of every pixel, raster order.
    #[must_use]
    pub fn bin_of(&self) -> &[u32] {
        &self.bin_of
    }

    /// Bin index of a colour that is present in the frame.
    #[must_use]
    pub fn index_of(&self, c: [u8; 3]) -> Option<usize> {
        let key = pack(c);
        let mut s = self.slot(key);
        loop {
            if self.slot_key[s] == key {
                return Some(self.slot_val[s] as usize);
            }
            if self.slot_key[s] == u32::MAX {
                return None;
            }
            s = (s + 1) & self.mask;
        }
    }

    /// Compute the Oklab of every bin, if it has not been computed yet.
    pub fn ensure_lab(&mut self) {
        if self.lab.len() == self.bins.len() {
            return;
        }
        self.lab.clear();
        self.lab.reserve(self.bins.len());
        for b in &self.bins {
            self.lab.push(oklab_srgb8(b.srgb));
        }
    }

    /// Oklab of every bin. Call [`Hist::ensure_lab`] first.
    #[must_use]
    pub fn lab(&self) -> &[[f32; 3]] {
        &self.lab
    }

    /// The frame's own colours as a palette, when there are few enough for
    /// one. The palette is in bin order, so `bin_of` *is* the index plane.
    #[must_use]
    pub fn exact_palette(&self, max: usize) -> Option<Vec<[u8; 3]>> {
        if self.bins.len() > max {
            return None;
        }
        Some(self.bins.iter().map(|b| b.srgb).collect())
    }

    /// Total weighted squared Oklab error of a palette on this histogram.
    /// Requires [`Hist::ensure_lab`].
    #[must_use]
    pub fn palette_cost(&self, pal: &super::nn::NnIndex) -> f64 {
        let mut e = 0f64;
        for (lab, b) in self.lab.iter().zip(&self.bins) {
            e += f64::from(pal.nearest(*lab).1) * f64::from(b.count);
        }
        e
    }
}
