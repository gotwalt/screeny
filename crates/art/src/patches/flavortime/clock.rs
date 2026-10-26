//! The time, as a shape on the panel for the fire to burn.
//!
//! The owner, 2026-10-26: "put a 3d voxel numeric read of the time ... in
//! the flames", and then "make it conceptually inverted: numerals are the
//! fuel".
//!
//! The numerals are drawn here as a 64 x 32 bitmap in LED space, from a
//! 3 x 5 cell font stroked to a size and weight ([`Font`]). The shader
//! extrudes it along the camera's rays through a slab of the firebox's
//! depth, so the fuel it holds lines up with the LEDs exactly from where
//! the camera stands. It is not drawn; only the fire shows it.

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

/// LEDs across and down a piece of fuel, when the numerals collapse.
const TILE: usize = 3;

/// How the numerals are drawn: their height and their strokes' width, LEDs.
/// Each font cell's centre is a point of a skeleton, joined to the lit
/// cells beside it, and every LED within half the weight of it (by the
/// larger of across and down, so the strokes stay square) is lit. At 15
/// tall and weight 3 that is the font cell for cell. The weight is at most
/// a cell ([`Font::weight`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
    pub height: f32,
    pub weight: f32,
}

impl Font {
    #[cfg(test)]
    pub const SHIPPED: Font = Font { height: 15.0, weight: 3.0 };

    /// LEDs per font cell for `text`: the height's, or less if the time,
    /// with a column spare each side, would not fit across the panel at it.
    fn cell(&self, text: &str) -> f32 {
        let cells: f32 = text.chars().map(|c| if c == ':' { 1.0 } else { 3.0 }).sum();
        let gaps = text.chars().count().saturating_sub(1) as f32;
        // One LED more for the strokes snapping onto LEDs.
        let fit = (W as f32 - 3.0 - gaps * self.gap()) / cells;
        (self.height / 5.0).min(fit).max(1.0)
    }

    /// LEDs between characters' cells: by the height asked for, so that
    /// fitting the time across the panel does not move it.
    fn gap(&self) -> f32 {
        (0.67 * self.height / 5.0).round().max(2.0)
    }

    /// The strokes' width at `cell`: no wider than a cell, or they fill the
    /// holes in 0, 4, 6, 8 and 9.
    fn weight(&self, cell: f32) -> f32 {
        self.weight.min(cell.floor()).max(1.0)
    }
}

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
    /// Whether it blows apart instead of collapsing: the pieces flung out
    /// and up from the time's middle, burning out as they fly.
    pub burst: bool,
    /// How far below its place it is, LEDs: rising into place.
    pub lift: f32,
}

impl Shape {
    pub const WHOLE: Shape = Shape { reveal: 1.0, melt: 0.0, salt: 0, burst: false, lift: 0.0 };
}

/// Where the numerals stand on the panel, LEDs: left column, one past the
/// right, top row, one past the bottom; how much of them has fallen out of
/// the bottom of the frame, into the woodpile, as they collapse; and the
/// pieces of them in motion (collapsing, flung or rising), which burn
/// without having to catch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub left: usize,
    pub right: usize,
    pub top: usize,
    pub bottom: usize,
    pub fallen: f32,
    pub moving: Mask,
}

/// Each character of `text` on the panel: its first LED column and width.
pub fn spans(text: &str, font: Font) -> Vec<(usize, usize)> {
    let cell = font.cell(text);
    let gap = font.gap();
    let width = |c: char| if c == ':' { cell } else { 3.0 * cell };
    let n = text.chars().count();
    let total = text.chars().map(width).sum::<f32>() + gap * n.saturating_sub(1) as f32;
    let mut x0 = ((W as f32 - total) / 2.0).max(0.0);
    text.chars()
        .map(|c| {
            let at = (x0.round() as usize, width(c).round() as usize);
            x0 += width(c) + gap;
            at
        })
        .collect()
}

/// A skeleton point's LED coordinate: on an LED's centre when the weight is
/// odd, between two when it is even, so the strokes land whole on LEDs.
fn snap(v: f32, weight: f32) -> f32 {
    if (weight.round() as i32) % 2 == 1 {
        v.floor() + 0.5
    } else {
        v.round()
    }
}

/// Character `c`'s lit LEDs, its font box's top-left at (`x0`, `top`).
fn stroke(c: char, x0: f32, top: f32, cell: f32, weight: f32, bits: &mut [[bool; W]; H]) {
    let rows: [u8; 5] = match c {
        ':' => [0, 1, 0, 1, 0],
        d => d.to_digit(10).map_or([0; 5], |d| DIGITS[d as usize]),
    };
    // The colon is one cell wide: its dots sit in that cell.
    let cols = if c == ':' { 1 } else { 3 };
    let lit = |cx: i32, cy: i32| (0..cols).contains(&cx) && (0..5).contains(&cy) && rows[cy as usize] >> (if c == ':' { 0 } else { cx }) & 1 == 1;
    let centre = |cx: i32, cy: i32| (snap(x0 + (cx as f32 + 0.5) * cell, weight), snap(top + (cy as f32 + 0.5) * cell, weight));
    let mut segments = Vec::new();
    for cy in 0..5 {
        for cx in 0..cols {
            if !lit(cx, cy) {
                continue;
            }
            let a = centre(cx, cy);
            segments.push((a, a));
            for (dx, dy) in [(1, 0), (0, 1)] {
                if lit(cx + dx, cy + dy) {
                    segments.push((a, centre(cx + dx, cy + dy)));
                }
            }
        }
    }
    let half = weight / 2.0 + 1e-3;
    for (y, row) in bits.iter_mut().enumerate() {
        for (x, b) in row.iter_mut().enumerate() {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Every segment is along a row or a column.
            *b |= segments.iter().any(|&((ax, ay), (bx, by))| {
                let dx = (ax.min(bx) - px).max(px - ax.max(bx)).max(0.0);
                let dy = (ay.min(by) - py).max(py - ay.max(by)).max(0.0);
                dx.max(dy) <= half
            });
        }
    }
}

/// Two masks together.
pub fn both(a: &Mask, b: &Mask) -> Mask {
    let mut out = *a;
    for (o, w) in out.iter_mut().flatten().zip(b.iter().flatten()) {
        *o |= w;
    }
    out
}

/// `text` drawn across the middle of the panel in `font`, its middle row
/// at `middle` (an LED row, 0 at the top), each character caught and
/// collapsed as far as its shape says: `shapes[i]` for character `i`, or
/// one shape for all.
pub fn mask(text: &str, middle: f32, font: Font, shapes: &[Shape]) -> (Mask, Bounds) {
    let cell = font.cell(text);
    let tall = 5.0 * cell;
    let top = (middle - tall / 2.0).round().clamp(0.0, (H as f32 - tall).max(0.0));
    let spans = spans(text, font);
    let left = spans.first().map_or(0, |s| s.0);
    let right = spans.last().map_or(0, |s| (s.0 + s.1).min(W));
    let bottom = ((top + tall).round() as usize).min(H);
    let mut bounds = Bounds { left, right, top: top as usize, bottom, fallen: 0.0, moving: [[0; 4]; 16] };
    let mut bits = [[false; W]; H];
    let mut moving = [[false; W]; H];
    let (mut pieces_down, mut pieces_all) = (0.0, 0.0);
    let gap = font.gap();
    let mut x0 = spans.first().map_or(0.0, |s| s.0 as f32);
    for (i, c) in text.chars().enumerate() {
        let shape = shapes[if shapes.len() == 1 { 0 } else { i }];
        let mut own = [[false; W]; H];
        stroke(c, x0, top + shape.lift.max(0.0).round(), cell, font.weight(cell), &mut own);
        x0 += if c == ':' { cell } else { 3.0 * cell } + gap;
        if shape.melt > 0.0 {
            // Pieces of fuel: the character's LEDs in tiles.
            let mut pieces = Vec::new();
            for ty in (0..H).step_by(TILE) {
                for tx in (0..W).step_by(TILE) {
                    let mut lit = [false; TILE * TILE];
                    for (k, l) in lit.iter_mut().enumerate() {
                        let (x, y) = (tx + k % TILE, ty + k / TILE);
                        *l = x < W && y < H && own[y][x];
                    }
                    if lit.iter().any(|&l| l) {
                        pieces.push((tx, ty, lit));
                    }
                }
            }
            let salt = shape.salt ^ (i as u32).wrapping_mul(0x9e37_79b9);
            let (down, fallen) = collapse(&pieces, &bounds, shape.melt.clamp(0.0, 1.0) * COLLAPSE, salt, shape.burst);
            for ((row, mrow), d) in bits.iter_mut().zip(moving.iter_mut()).zip(down) {
                for ((b, m), d) in row.iter_mut().zip(mrow.iter_mut()).zip(d) {
                    *b |= d;
                    *m |= d;
                }
            }
            pieces_down += fallen * pieces.len() as f32;
            pieces_all += pieces.len() as f32;
            continue;
        }
        // Caught from the bottom up: rows above the front hold no fuel yet.
        let front = (bottom as f32 - shape.reveal.clamp(0.0, 1.0) * tall).ceil().max(0.0) as usize;
        let rising = shape.lift >= 0.5;
        for ((row, mrow), own) in bits.iter_mut().zip(moving.iter_mut()).zip(own).skip(front) {
            for ((b, m), o) in row.iter_mut().zip(mrow.iter_mut()).zip(own) {
                *b |= o;
                *m |= o && rising;
            }
        }
    }
    if pieces_all > 0.0 {
        bounds.fallen = pieces_down / pieces_all;
    }
    bounds.moving = packed(&moving);
    (packed(&bits), bounds)
}

/// LEDs as a [`Mask`].
fn packed(bits: &[[bool; W]; H]) -> Mask {
    let mut out = [[0u32; 4]; 16];
    for (y, row) in bits.iter().enumerate() {
        for (x, &on) in row.iter().enumerate() {
            if on {
                let w = y * 2 + x / 32;
                out[w / 4][w % 4] |= 1 << (x % 32);
            }
        }
    }
    out
}

/// How long the numerals take to collapse, seconds.
pub const COLLAPSE: f32 = 3.5;

/// The numerals `t` seconds into collapsing, as a pile of fuel does (the
/// owner, 2026-10-26: "i'd like it to look more realistic, like a pile of
/// fuel collapsing"). Each tile of the numerals is a piece. LEDs burn through here and
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
///
/// With `burst` it blows apart instead (2026-10-26, the hour turning over:
/// "the old format probably blows up"): every piece goes at once, flung
/// out from the time's middle and up, arcing down under gravity and
/// burning out as it flies, as sparks do.
fn collapse(pieces: &[(usize, usize, [bool; TILE * TILE])], at: &Bounds, t: f32, salt: u32, burst: bool) -> ([[bool; W]; H], f32) {
    // LEDs per second squared: slow enough to watch, fast enough to fall.
    const GRAVITY: f32 = 110.0;
    const BURN_THROUGH: f32 = 0.5;
    let c = TILE as f32;
    let mut dice = Rng::new(u64::from(salt).wrapping_mul(0x2545_f491_4f6c_dd1d) ^ 0xc011_a95e);
    let fails_at = at.left as f32 + dice.f32() * (at.right - at.left) as f32;
    let mut bits = [[false; W]; H];
    let mut fallen = 0;
    let middle = (0.5 * (at.left + at.right) as f32, 0.5 * (at.top + at.bottom) as f32);
    for &(x, y, lit) in pieces {
        let (x, y) = (x as f32, y as f32);
        let mut burns = [0.0; TILE * TILE];
        let (drop, vx, vy) = if burst {
            let (dx, dy) = (x + c / 2.0 - middle.0, y + c / 2.0 - middle.1);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let speed = dice.range(18.0, 42.0);
            for b in &mut burns {
                *b = dice.range(0.2, 0.8);
            }
            (dice.range(0.0, 0.08), dx / len * speed + dice.range(-4.0, 4.0), dy / len * speed - dice.range(15.0, 35.0))
        } else {
            let from = (x + c / 2.0 - fails_at) / c;
            let height = (at.bottom as f32 - (y + c)) / c;
            for b in &mut burns {
                // A few burn through early; the rest as it comes down.
                *b = if dice.f32() < 0.12 { dice.range(0.1, BURN_THROUGH + 0.4) } else { dice.range(1.4, COLLAPSE - 0.2) };
            }
            (BURN_THROUGH + 0.04 * from.abs() + 0.08 * height + dice.range(0.0, 0.25), from.signum() * dice.range(0.5, 5.0) + dice.range(-1.5, 1.5), 0.0)
        };
        // Flying freely from `drop`: no step-by-step needed.
        let fall = (t - drop).max(0.0);
        let (x, y) = (x + vx * fall, y + vy * fall + 0.5 * GRAVITY * fall * fall);
        if y >= H as f32 || x <= -c || x >= W as f32 || t >= burns.iter().copied().fold(0.0, f32::max) {
            fallen += 1;
            continue;
        }
        let (x, y) = (x.round() as i32, y.round() as i32);
        for (k, &b) in burns.iter().enumerate() {
            let (px, py) = (x + (k % TILE) as i32, y + (k / TILE) as i32);
            if lit[k] && t < b && (0..W as i32).contains(&px) && (0..H as i32).contains(&py) {
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
        let (m, b) = mask("88:88", 20.0, Font::SHIPPED, &[Shape::WHOLE]);
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
        assert_eq!(b.moving, [[0; 4]; 16], "nothing standing is moving");
    }

    fn lit(m: &Mask) -> Vec<(usize, usize)> {
        (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| at(m, x, y)).collect()
    }

    #[test]
    fn the_numerals_catch_from_the_bottom_up() {
        let (half, b) = mask("10:42", 20.0, Font::SHIPPED, &[Shape { reveal: 0.5, ..Shape::WHOLE }]);
        let ys: Vec<usize> = lit(&half).iter().map(|p| p.1).collect();
        assert!(!ys.is_empty());
        assert!(ys.iter().all(|&y| y >= b.top + 7), "only the lower half: {:?}", ys.iter().min());
        assert!(lit(&mask("10:42", 20.0, Font::SHIPPED, &[Shape { reveal: 0.0, ..Shape::WHOLE }]).0).is_empty());
    }

    #[test]
    fn the_numerals_collapse_out_of_the_bottom_of_the_frame() {
        let at = |melt: f32| mask("10:42", 20.0, Font::SHIPPED, &[Shape { melt, ..Shape::WHOLE }]);
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
        assert_ne!(at(0.4).0, mask("10:42", 20.0, Font::SHIPPED, &[Shape { melt: 0.4, salt: 99, ..Shape::WHOLE }]).0);
    }

    #[test]
    fn a_burst_flings_the_pieces_out_and_they_burn_away() {
        let at_t = |melt: f32| mask("9:59", 16.0, Font::SHIPPED, &[Shape { melt, burst: true, ..Shape::WHOLE }]);
        let (whole, _) = mask("9:59", 16.0, Font::SHIPPED, &[Shape::WHOLE]);
        let span = |m: &Mask| {
            let xs: Vec<usize> = lit(m).iter().map(|p| p.0).collect();
            xs.iter().max().unwrap_or(&0) - xs.iter().min().unwrap_or(&0)
        };
        let top = |m: &Mask| lit(m).iter().map(|p| p.1).min().unwrap_or(H);
        // A tenth of a second in, it is flying apart: wider and higher.
        let (early, _) = at_t(0.1 / COLLAPSE);
        assert!(span(&early) > span(&whole), "{} vs {}", span(&early), span(&whole));
        assert!(top(&early) < top(&whole));
        // What is flying is all moving.
        let (_, b) = at_t(0.1 / COLLAPSE);
        assert_eq!(b.moving, early);
        // Gone within a second: burnt out or out of the frame.
        let (gone, b) = at_t(1.0 / COLLAPSE);
        assert!(lit(&gone).is_empty());
        assert_eq!(b.fallen, 1.0);
    }

    #[test]
    fn size_and_weight_change_the_digits() {
        let draw = |height: f32, weight: f32| mask("10:48", 16.0, Font { height, weight }, &[Shape::WHOLE]).0;
        let rows = |m: &Mask| {
            let ys: Vec<usize> = lit(m).iter().map(|p| p.1).collect();
            ys.iter().max().unwrap() - ys.iter().min().unwrap() + 1
        };
        let cols = |m: &Mask| {
            let xs: Vec<usize> = lit(m).iter().map(|p| p.0).collect();
            (*xs.iter().min().unwrap(), *xs.iter().max().unwrap())
        };
        assert_eq!(rows(&draw(15.0, 3.0)), 15);
        assert!(rows(&draw(10.0, 2.0)) <= 11);
        assert!(rows(&draw(20.0, 3.0)) >= 17, "taller, even if narrowed to fit");
        // The biggest still fits across the panel, with a column spare.
        let (l, r) = cols(&draw(20.0, 5.0));
        assert!(l >= 1 && r <= W - 2, "{l}..{r}");
        // Lighter strokes, fewer LEDs; and every weight still reads as five
        // separate characters (columns with nothing lit between them).
        assert!(lit(&draw(15.0, 1.0)).len() < lit(&draw(15.0, 3.0)).len() / 2);
        for weight in [1.0, 2.0, 3.0, 4.0, 5.0] {
            let m = draw(15.0, weight);
            let (l, r) = cols(&m);
            let runs = (l..=r).map(|x| (0..H).any(|y| at(&m, x, y))).collect::<Vec<_>>().windows(2).filter(|w| w[0] && !w[1]).count();
            assert_eq!(runs, 4, "weight {weight}: four gaps between five characters");
        }
    }

    #[test]
    fn the_mask_stays_on_the_panel() {
        let (m, _) = mask("12:59", 100.0, Font::SHIPPED, &[Shape::WHOLE]);
        assert!((0..W).any(|x| at(&m, x, H - 1)));
        // Lifted far enough, a character is below the frame altogether.
        let (m, _) = mask("12:59", 16.0, Font::SHIPPED, &[Shape { lift: 30.0, ..Shape::WHOLE }]);
        assert!(lit(&m).is_empty());
        let (m, _) = mask("12:59", -100.0, Font::SHIPPED, &[Shape::WHOLE]);
        assert!((0..W).any(|x| at(&m, x, 0)));
    }
}
