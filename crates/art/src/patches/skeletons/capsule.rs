//! A supersampled coverage buffer, drawn into with tapered capsules and
//! discs, in screen space. Reused wholesale from `flock`'s `Coverage`
//! (`patches/flock/mod.rs`) but with two differences this patch needs and
//! flock does not: a capsule may taper (a different radius at each end, for
//! a bone that is fatter at the root) and the ink itself may differ end to
//! end (for the light's lit side and shadow side). Kept separate from
//! flock's copy so the two patches share no file to merge; card 333 (`thing`)
//! is expected to take this one as it stands.

use crate::color::smoothstep;

/// Coverage samples per panel pixel per axis. Three, as flock found: enough
/// to anti-alias a bone's edge without smearing it, and cheap at the small
/// number of capsules a couple of skeletons draw.
pub const SUPERSAMPLE: usize = 3;

pub struct Coverage {
    ss: usize,
    w: usize,
    h: usize,
    buf: Vec<f32>,
}

impl Coverage {
    pub fn new(ss: usize, panel_w: usize, panel_h: usize) -> Coverage {
        let (w, h) = (panel_w * ss, panel_h * ss);
        Coverage { ss, w, h, buf: vec![0.0; w * h] }
    }

    /// A tapered, anti-aliased capsule from `a` (radius `ra`, ink `ink_a`) to
    /// `b` (radius `rb`, ink `ink_b`), painted over what is already there.
    /// The radius and the ink both vary linearly along the bone's length,
    /// which is what makes a bone read as lit on one side and taper towards
    /// a joint on the other.
    pub fn capsule(&mut self, a: (f32, f32), b: (f32, f32), ra: f32, rb: f32, ink_a: f32, ink_b: f32) {
        let ss = self.ss as f32;
        let aa = 0.5 / ss;
        let pad = ra.max(rb) + aa;
        let lo_x = (((a.0.min(b.0) - pad) * ss).floor().max(0.0)) as usize;
        let hi_x = (((a.0.max(b.0) + pad) * ss).ceil().max(0.0) as usize).min(self.w);
        let lo_y = (((a.1.min(b.1) - pad) * ss).floor().max(0.0)) as usize;
        let hi_y = (((a.1.max(b.1) + pad) * ss).ceil().max(0.0) as usize).min(self.h);
        if lo_x >= hi_x || lo_y >= hi_y {
            return;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = (dx * dx + dy * dy).max(1e-9);
        for j in lo_y..hi_y {
            let py = (j as f32 + 0.5) / ss;
            for i in lo_x..hi_x {
                let px = (i as f32 + 0.5) / ss;
                let t = (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0);
                let (ex, ey) = (px - a.0 - dx * t, py - a.1 - dy * t);
                let d = (ex * ex + ey * ey).sqrt();
                let r = ra + (rb - ra) * t;
                let cover = smoothstep(r + aa, r - aa, d);
                if cover > 0.0 {
                    let ink = ink_a + (ink_b - ink_a) * t;
                    let cell = &mut self.buf[j * self.w + i];
                    *cell += (ink - *cell) * cover;
                }
            }
        }
    }

    /// Mean coverage over one panel pixel.
    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> f32 {
        let mut sum = 0.0;
        for j in 0..self.ss {
            let row = (y * self.ss + j) * self.w + x * self.ss;
            sum += self.buf[row..row + self.ss].iter().sum::<f32>();
        }
        sum / (self.ss * self.ss) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capsule_covers_its_own_middle_fully() {
        let mut c = Coverage::new(4, 16, 16);
        c.capsule((2.0, 8.0), (14.0, 8.0), 1.5, 1.5, 1.0, 1.0);
        assert!(c.pixel(8, 8) > 0.98, "{}", c.pixel(8, 8));
        assert!(c.pixel(8, 15) < 0.02, "far away should be untouched");
    }

    #[test]
    fn a_later_capsule_overwrites_a_nearer_ink_where_it_is_more_opaque() {
        let mut c = Coverage::new(4, 16, 16);
        c.capsule((8.5, 8.5), (8.5, 8.5), 3.0, 3.0, 0.2, 0.2);
        c.capsule((8.5, 8.5), (8.5, 8.5), 1.0, 1.0, 0.9, 0.9);
        assert!(c.pixel(8, 8) > 0.85, "{}", c.pixel(8, 8));
    }

    #[test]
    fn ink_and_radius_taper_along_the_capsule() {
        let mut c = Coverage::new(6, 20, 8);
        c.capsule((2.0, 4.0), (18.0, 4.0), 2.5, 0.5, 1.0, 0.1);
        assert!(c.pixel(3, 4) > c.pixel(17, 4), "wide, bright end should read stronger than the thin, dim end");
    }
}
