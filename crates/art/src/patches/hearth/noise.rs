//! Small seeded value noise, in one and two dimensions, for the things in the
//! hearth that wander slowly: each log's own strength, the whole fire's
//! breathing over minutes, the calm drift of the air, and where along a log
//! the fuel happens to be coming through.
//!
//! Lattice value noise with smootherstep interpolation, summed over octaves.
//! Hand-rolled (no noise crate in the workspace) and deliberately its own copy
//! rather than `leaves::wind`'s, which is behind the `gpu` feature and must
//! keep drawing byte-identically.

/// An avalanche hash: any integer in, an unrelated-looking integer out.
fn hash(mut n: u32) -> u32 {
    n ^= n >> 16;
    n = n.wrapping_mul(0x7feb_352d);
    n ^= n >> 15;
    n = n.wrapping_mul(0x846c_a68b);
    n ^= n >> 16;
    n
}

fn lattice(ix: i32, iy: i32, salt: u32) -> f32 {
    let n = (ix as u32)
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add((iy as u32).wrapping_mul(0x85EB_CA6B))
        .wrapping_add(salt.wrapping_mul(0x27D4_EB2F));
    (hash(n) >> 8) as f32 / (1u32 << 24) as f32
}

fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Value noise at `(x, y)`, in `0..1`.
pub fn noise2(x: f32, y: f32, salt: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (ux, uy) = (fade(x - x0), fade(y - y0));
    let (ix, iy) = (x0 as i32, y0 as i32);
    let a = lattice(ix, iy, salt);
    let b = lattice(ix + 1, iy, salt);
    let c = lattice(ix, iy + 1, salt);
    let d = lattice(ix + 1, iy + 1, salt);
    let top = a + (b - a) * ux;
    let bot = c + (d - c) * ux;
    top + (bot - top) * uy
}

/// Value noise along one axis, in `0..1`.
pub fn noise1(x: f32, salt: u32) -> f32 {
    noise2(x, 0.5, salt)
}

/// Three octaves of [`noise2`], centred: roughly `-1..1`, usually within
/// `-0.6..0.6`.
pub fn fbm2(x: f32, y: f32, salt: u32) -> f32 {
    let mut sum = 0.0;
    let (mut amp, mut freq, mut norm) = (1.0, 1.0, 0.0);
    for o in 0..3u32 {
        sum += amp * (noise2(x * freq, y * freq, salt.wrapping_add(o * 977)) * 2.0 - 1.0);
        norm += amp;
        amp *= 0.5;
        freq *= 2.03;
    }
    sum / norm
}

/// [`fbm2`] along one axis.
pub fn fbm1(x: f32, salt: u32) -> f32 {
    fbm2(x, 0.37, salt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_bounded_and_continuous() {
        let mut prev = noise1(0.0, 3);
        for i in 1..4000 {
            let x = i as f32 * 0.01;
            let v = noise1(x, 3);
            assert!((0.0..=1.0).contains(&v));
            assert!((v - prev).abs() < 0.05, "a jump at {x}");
            prev = v;
            assert!(fbm1(x, 9).abs() <= 1.0);
        }
    }
}
