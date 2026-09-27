//! The wind field: layered value noise in space and time, so groups of
//! leaves get pushed together rather than each drifting on its own.
//!
//! There is no noise crate in this workspace and no other patch needed one,
//! so this is a small hand-rolled lattice value noise (hash the eight corners
//! of a unit cube in x, y, time; smootherstep-interpolate) summed over three
//! octaves (fbm). It is not Perlin/Simplex gradient noise - the visual
//! difference at this size is not worth a second dependency - but it is
//! genuinely layered in space and in time, deterministic from the patch
//! seed, and cheap enough to sample per leaf per physics step.

/// A 32-bit avalanche hash (this is the well-known "triple xorshift-multiply"
/// mixer, e.g. used in various hash-map and noise implementations). Any
/// integer in, an unrelated-looking integer out - that is all a lattice noise
/// needs from its hash.
fn hash_u32(mut n: u32) -> u32 {
    n ^= n >> 16;
    n = n.wrapping_mul(0x7feb_352d);
    n ^= n >> 15;
    n = n.wrapping_mul(0x846c_a68b);
    n ^= n >> 16;
    n
}

/// One lattice point's pseudo-random value in `0..1`, for the integer cell
/// `(ix, iy, iz)` under `salt` (which noise channel this is - so the x-push,
/// y-push and gust envelope below do not correlate).
fn lattice(ix: i32, iy: i32, iz: i32, salt: u32) -> f32 {
    let n = (ix as u32)
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add((iy as u32).wrapping_mul(0x85EB_CA6B))
        .wrapping_add((iz as u32).wrapping_mul(0xC2B2_AE35))
        .wrapping_add(salt.wrapping_mul(0x27D4_EB2F));
    (hash_u32(n) >> 8) as f32 / (1u32 << 24) as f32
}

/// Smootherstep: zero first and second derivative at both ends, so the noise
/// has no visible seams between cells and no kinks a moving leaf could feel
/// as a jolt.
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Trilinear-interpolated value noise at a real-valued `(x, y, z)`, `0..1`.
fn value_noise(x: f32, y: f32, z: f32, salt: u32) -> f32 {
    let (x0, y0, z0) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
    let (ux, uy, uz) = (fade(x - x0 as f32), fade(y - y0 as f32), fade(z - z0 as f32));
    let l = |dx: i32, dy: i32, dz: i32| lattice(x0 + dx, y0 + dy, z0 + dz, salt);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(l(0, 0, 0), l(1, 0, 0), ux);
    let x10 = lerp(l(0, 1, 0), l(1, 1, 0), ux);
    let x01 = lerp(l(0, 0, 1), l(1, 0, 1), ux);
    let x11 = lerp(l(0, 1, 1), l(1, 1, 1), ux);
    let y0v = lerp(x00, x10, uy);
    let y1v = lerp(x01, x11, uy);
    lerp(y0v, y1v, uz)
}

/// Three octaves, each double the frequency and half the weight of the last:
/// broad, slow structures under fine, fast ones, which is what "layered" is
/// asking for. `0..1`.
fn fbm(x: f32, y: f32, z: f32, salt: u32) -> f32 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for o in 0..3u32 {
        sum += amp * value_noise(x * freq, y * freq, z * freq, salt.wrapping_add(o.wrapping_mul(101)));
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

/// How far apart, in panel widths, the noise field's own structures are - the
/// size of a "pocket" of moving air a handful of leaves share. Small enough
/// that the 64 x 32 panel sees several pockets at once, large enough that
/// neighbouring leaves are pushed together rather than independently.
const SCALE: f32 = 1.0 / 20.0;
/// How many seconds a gust pocket takes to evolve into a different one.
const TIME_SCALE: f32 = 1.0 / 6.0;

pub struct Wind {
    /// Salted from the patch seed so two seeds fly through different air, and
    /// offset from zero so a seed of `0` is not a degenerate all-zero field.
    seed: u32,
}

impl Wind {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Wind { seed: (seed as u32) ^ 0x6c65_6166 }
    }

    /// The wind's velocity at `(x, y)` (panel coordinates, y down) at time
    /// `t` seconds, given the patch's `mean` wind and `gusts` strength.
    ///
    /// A slow, broad envelope (half the frequency of the push itself) decides
    /// how strong the gust is right now and here - mostly modest, with the
    /// occasional strong pocket the brief asks for - so gustiness is not a
    /// steady shimmer but comes and goes in patches of space and time.
    #[must_use]
    pub fn at(&self, x: f32, y: f32, t: f32, mean: f32, gusts: f32) -> (f32, f32) {
        let (sx, sy, st) = (x * SCALE, y * SCALE, t * TIME_SCALE);
        let envelope = fbm(sx * 0.4, sy * 0.4, st * 0.4, self.seed ^ 0x9E37_79B9);
        let gust = gusts * (0.25 + 1.5 * envelope * envelope);

        let push = fbm(sx, sy, st, self.seed) - 0.5;
        let lift = fbm(sx * 1.3 + 11.0, sy * 1.3, st * 1.1 + 7.0, self.seed ^ 0x1234_ABCD) - 0.5;
        // Gusts push mostly sideways (the brief: "a gusty field... pushes
        // groups of leaves sideways together") with a much smaller vertical
        // component - real gusts do lift and drop things a little, but a
        // wind that shoved leaves up and down as hard as it shoves them
        // sideways would read as bouncing, not blowing.
        (mean + gust * push * 5.0, gust * lift * 1.6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_field_is_bounded_and_deterministic() {
        let w = Wind::new(7);
        let w2 = Wind::new(7);
        let w3 = Wind::new(8);
        let mut saw_difference = false;
        for i in 0..200 {
            let (x, y, t) = (i as f32 * 1.7, i as f32 * 0.9, i as f32 * 0.3);
            let a = w.at(x, y, t, 0.8, 0.6);
            let b = w2.at(x, y, t, 0.8, 0.6);
            assert_eq!(a, b, "the field is a pure function of position, time and seed");
            assert!(a.0.is_finite() && a.1.is_finite());
            assert!(a.0.abs() < 20.0 && a.1.abs() < 20.0, "wind should not blow up: {a:?}");
            if w3.at(x, y, t, 0.8, 0.6) != a {
                saw_difference = true;
            }
        }
        assert!(saw_difference, "another seed should fly through different air");
    }
}
