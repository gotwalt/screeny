//! The time, as a shape on the panel for the fire to go round.
//!
//! The owner, 2026-10-26: "put a 3d voxel numeric read of the time as a
//! non-flammable object interfering with the flames. On an individual frame
//! you might not be able to see it, but over 30fps the behavior of the flame
//! simulation should make it visible."
//!
//! The numerals are drawn here as a 64 x 32 bitmap in LED space, chunky
//! 3 x 5 cells. The shader extrudes it along the camera's rays through a slab
//! of the firebox's depth, so the solid it makes lines up with the LEDs
//! exactly from where the camera stands, and is a wall like the logs: no gas
//! in it, nothing through it. It is not drawn; only the fire shows it.

use crate::frame::{H, W};
use crate::rng::Rng;

/// The numerals, 3 cells across and 5 down; bit `x` of a row is cell `x`
/// from the left.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b011, 0b010, 0b010, 0b111],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b100, 0b111],
    [0b101, 0b101, 0b111, 0b100, 0b100],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b101, 0b111],
    [0b111, 0b100, 0b100, 0b100, 0b100],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b100, 0b111],
];

/// LEDs per font cell: a numeral is 9 x 15 LEDs, its strokes 3 wide - at
/// the coarsest grid (a quarter cell per design unit) still two cells of
/// wall, so the gas cannot leak through a stroke.
const CELL: usize = 3;
/// LEDs between numerals, and either side of the colon.
const GAP: usize = 2;

/// The panel as bits, two `u32` words to a row (`x` < 32 in the first),
/// four words to a uniform `vec4`: as `glyph` in the shader's `Params`.
pub type Mask = [[u32; 4]; 16];

/// "H:MM" or "HH:MM" for `now` (seconds on [`crate::patch::local_now`]'s
/// scale), 12- or 24-hour.
pub fn text(now: f64, hours24: bool) -> String {
    let minutes = (now / 60.0).floor().rem_euclid(24.0 * 60.0) as u32;
    let (h, m) = (minutes / 60, minutes % 60);
    let h = if hours24 {
        h
    } else {
        match h % 12 {
            0 => 12,
            h => h,
        }
    };
    if hours24 {
        format!("{h:02}:{m:02}")
    } else {
        format!("{h}:{m:02}")
    }
}

/// How far through its showing the time is, for [`mask`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    /// How much of the numerals has caught, from the bottom up: 0 none, 1
    /// all of them.
    pub reveal: f32,
    /// How far they have collapsed ([`collapse`]): 0 standing, 1 fallen
    /// out of the frame, over [`COLLAPSE`] seconds.
    pub melt: f32,
    /// Which collapse: where it gives way, how the pieces fall and burn.
    pub salt: u32,
}

impl Shape {
    pub const WHOLE: Shape = Shape { reveal: 1.0, melt: 0.0, salt: 0 };
}

/// Where the numerals stand on the panel, LEDs: left column, one past the
/// right, top row, one past the bottom; and how much of them has fallen
/// out of the bottom of the frame, into the woodpile, as they collapse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub left: usize,
    pub right: usize,
    pub top: usize,
    pub bottom: usize,
    pub fallen: f32,
}

/// Each character of `text` on the panel: its first LED column and width.
pub fn spans(text: &str) -> Vec<(usize, usize)> {
    let width = |c: char| if c == ':' { CELL } else { 3 * CELL };
    let n = text.chars().count();
    let total = text.chars().map(width).sum::<usize>() + GAP * n.saturating_sub(1);
    let mut x0 = W.saturating_sub(total) / 2;
    text.chars()
        .map(|c| {
            let at = (x0, width(c));
            x0 += width(c) + GAP;
            at
        })
        .collect()
}

/// Two masks together.
pub fn both(a: &Mask, b: &Mask) -> Mask {
    let mut out = *a;
    for (o, w) in out.iter_mut().flatten().zip(b.iter().flatten()) {
        *o |= w;
    }
    out
}

/// `text` drawn across the middle of the panel, its middle row at `middle`
/// (an LED row, 0 at the top), each character caught and collapsed as far
/// as its shape says: `shapes[i]` for character `i`, or one shape for all.
pub fn mask(text: &str, middle: f32, shapes: &[Shape]) -> (Mask, Bounds) {
    let tall = 5 * CELL;
    let top = (middle - tall as f32 / 2.0).round().clamp(0.0, (H - tall) as f32) as usize;
    let bottom = top + tall;
    let spans = spans(text);
    let left = spans.first().map_or(0, |s| s.0);
    let right = spans.last().map_or(0, |s| (s.0 + s.1).min(W));
    let mut bounds = Bounds { left, right, top, bottom, fallen: 0.0 };
    let mut bits = [[false; W]; H];
    let (mut pieces_down, mut pieces_all) = (0.0, 0.0);
    for (i, (c, &(x0, _))) in text.chars().zip(&spans).enumerate() {
        let shape = shapes[if shapes.len() == 1 { 0 } else { i }];
        let rows: [u8; 5] = match c {
            ':' => [0, 1, 0, 1, 0],
            d => d.to_digit(10).map_or([0; 5], |d| DIGITS[d as usize]),
        };
        // Each font cell is a piece of fuel, for the collapse: its top-left LED.
        let mut pieces = Vec::new();
        for (ry, row) in rows.iter().enumerate() {
            for rx in 0..3 {
                if row >> rx & 1 == 1 {
                    pieces.push((x0 + rx * CELL, top + ry * CELL));
                }
            }
        }
        if shape.melt > 0.0 {
            let salt = shape.salt ^ (i as u32).wrapping_mul(0x9e37_79b9);
            let (down, fallen) = collapse(&pieces, &bounds, shape.melt.clamp(0.0, 1.0) * COLLAPSE, salt);
            for (row, d) in bits.iter_mut().zip(down) {
                for (b, d) in row.iter_mut().zip(d) {
                    *b |= d;
                }
            }
            pieces_down += fallen * pieces.len() as f32;
            pieces_all += pieces.len() as f32;
            continue;
        }
        // Caught from the bottom up: rows above the front hold no fuel yet.
        let front = bottom as f32 - shape.reveal.clamp(0.0, 1.0) * tall as f32;
        for &(x, y) in &pieces {
            for row in bits.iter_mut().take(y + CELL).skip(y.max(front.ceil().max(0.0) as usize)) {
                row[x.min(W)..(x + CELL).min(W)].fill(true);
            }
        }
    }
    if pieces_all > 0.0 {
        bounds.fallen = pieces_down / pieces_all;
    }
    let mut out = [[0u32; 4]; 16];
    for (y, row) in bits.iter().enumerate() {
        for (x, &on) in row.iter().enumerate() {
            if on {
                let w = y * 2 + x / 32;
                out[w / 4][w % 4] |= 1 << (x % 32);
            }
        }
    }
    (out, bounds)
}

/// How long the numerals take to collapse, seconds.
pub const COLLAPSE: f32 = 3.5;

/// The numerals `t` seconds into collapsing, as a pile of fuel does (the
/// owner, 2026-10-26: "i'd like it to look more realistic, like a pile of
/// fuel collapsing"). Each font cell is a piece. LEDs burn through here and
/// there first; then the base gives way at one place and the failure runs
/// out from it, each piece dropping under gravity when its turn comes -
/// the upper ones a beat after the ones under them - and drifting away from
/// where it gave way, out of the bottom of the frame to where the woodpile
/// is when the time is not showing (the owner: "the numbers should deform
/// to off the bottom of the screen, where the "fuel" is normally"). Every
/// LED burns out at its own time.
///
/// Also answers the share of the pieces that has fallen out of the frame.
/// Worked out afresh from `t`, so it is the same at any frame rate, and for
/// a given `salt` the same every time.
fn collapse(pieces: &[(usize, usize)], at: &Bounds, t: f32, salt: u32) -> ([[bool; W]; H], f32) {
    // LEDs per second squared: slow enough to watch, fast enough to fall.
    const GRAVITY: f32 = 110.0;
    const BURN_THROUGH: f32 = 0.5;
    let c = CELL as f32;
    let mut dice = Rng::new(u64::from(salt).wrapping_mul(0x2545_f491_4f6c_dd1d) ^ 0xc011_a95e);
    let fails_at = at.left as f32 + dice.f32() * (at.right - at.left) as f32;
    let mut bits = [[false; W]; H];
    let mut fallen = 0;
    for &(x, y) in pieces {
        let (x, y) = (x as f32, y as f32);
        let from = (x + c / 2.0 - fails_at) / c;
        let height = (at.bottom as f32 - (y + c)) / c;
        let drop = BURN_THROUGH + 0.04 * from.abs() + 0.08 * height + dice.range(0.0, 0.25);
        let vx = from.signum() * dice.range(0.5, 5.0) + dice.range(-1.5, 1.5);
        let mut burns = [0.0; CELL * CELL];
        for b in &mut burns {
            // A few burn through early; the rest as it comes down.
            *b = if dice.f32() < 0.12 { dice.range(0.1, BURN_THROUGH + 0.4) } else { dice.range(1.4, COLLAPSE - 0.2) };
        }
        // Falling freely from `drop`: no step-by-step needed.
        let fall = (t - drop).max(0.0);
        let (x, y) = (x + vx * fall, y + 0.5 * GRAVITY * fall * fall);
        if y >= H as f32 {
            fallen += 1;
            continue;
        }
        let (x, y) = (x.round() as i32, y.round() as i32);
        for (k, &b) in burns.iter().enumerate() {
            let (px, py) = (x + (k % CELL) as i32, y + (k / CELL) as i32);
            if t < b && (0..W as i32).contains(&px) && (0..H as i32).contains(&py) {
                bits[py as usize][px as usize] = true;
            }
        }
    }
    (bits, fallen as f32 / pieces.len().max(1) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(m: &Mask, x: usize, y: usize) -> bool {
        let w = y * 2 + x / 32;
        m[w / 4][w % 4] >> (x % 32) & 1 == 1
    }

    #[test]
    fn the_time_reads_both_ways() {
        let t = (13 * 3600 + 5 * 60 + 59) as f64;
        assert_eq!(text(t, true), "13:05");
        assert_eq!(text(t, false), "1:05");
        assert_eq!(text(30.0, false), "12:00");
        assert_eq!(text(30.0, true), "00:00");
        assert_eq!(text(-30.0, true), "23:59", "a day before the epoch is still a time of day");
    }

    #[test]
    fn the_mask_is_centred_and_the_digits_are_where_they_should_be() {
        let (m, b) = mask("88:88", 20.0, &[Shape::WHOLE]);
        let lit: Vec<(usize, usize)> = (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| at(&m, x, y)).collect();
        let (xs, ys): (Vec<_>, Vec<_>) = lit.iter().copied().unzip();
        let (l, r) = (*xs.iter().min().unwrap(), *xs.iter().max().unwrap());
        assert!(l.abs_diff(W - 1 - r) <= 1, "centred: {l}..{r}");
        assert_eq!(*ys.iter().min().unwrap(), 13);
        assert_eq!(*ys.iter().max().unwrap(), 27);
        // An 8's middle bar is solid, its holes empty.
        assert!(at(&m, l + 4, 20));
        assert!(!at(&m, l + 4, 17));
        assert_eq!((b.left, b.right - 1, b.top, b.bottom - 1, b.fallen), (l, r, 13, 27, 0.0));
    }

    fn lit(m: &Mask) -> Vec<(usize, usize)> {
        (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| at(m, x, y)).collect()
    }

    #[test]
    fn the_numerals_catch_from_the_bottom_up() {
        let (half, b) = mask("10:42", 20.0, &[Shape { reveal: 0.5, ..Shape::WHOLE }]);
        let ys: Vec<usize> = lit(&half).iter().map(|p| p.1).collect();
        assert!(!ys.is_empty());
        assert!(ys.iter().all(|&y| y >= b.top + 7), "only the lower half: {:?}", ys.iter().min());
        assert!(lit(&mask("10:42", 20.0, &[Shape { reveal: 0.0, ..Shape::WHOLE }]).0).is_empty());
    }

    #[test]
    fn the_numerals_collapse_out_of_the_bottom_of_the_frame() {
        let at = |melt: f32| mask("10:42", 20.0, &[Shape { melt, ..Shape::WHOLE }]);
        let (whole, _) = at(0.0);
        let low = |m: &Mask| lit(m).iter().map(|p| p.1).min().unwrap_or(H);
        // Early on, a few LEDs have burnt through and nothing has fallen.
        let (early, b) = at(0.08);
        assert_eq!(low(&early), low(&whole));
        assert!(lit(&early).len() < lit(&whole).len(), "some burnt through");
        assert_eq!(b.fallen, 0.0);
        // Partway, the tops have come down and some has left the frame.
        let (part, b) = at(0.55);
        assert!(low(&part) > low(&whole) + 4, "the tops have come down: {} vs {}", low(&part), low(&whole));
        assert!(b.fallen > 0.0 && b.fallen < 1.0, "some fallen: {}", b.fallen);
        // At the end it has all gone into the woodpile.
        let (end, b) = at(1.0);
        assert!(lit(&end).is_empty());
        assert_eq!(b.fallen, 1.0);
        // Another salt, another collapse.
        assert_ne!(at(0.4).0, mask("10:42", 20.0, &[Shape { melt: 0.4, salt: 99, ..Shape::WHOLE }]).0);
    }

    #[test]
    fn the_mask_stays_on_the_panel() {
        let (m, _) = mask("12:59", 100.0, &[Shape::WHOLE]);
        assert!((0..W).any(|x| at(&m, x, H - 1)));
        let (m, _) = mask("12:59", -100.0, &[Shape::WHOLE]);
        assert!((0..W).any(|x| at(&m, x, 0)));
    }
}
