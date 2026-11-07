//! The instrument: a 3 x 8 grid of two-handed dials, the digits their hands
//! draw, and how a dial that is not part of a digit rests.
//!
//! No picture here and no clock. Hands are angles; what turns them is
//! [`crate::engine`] and what draws them is `crates/art` on the host and
//! [`crate::raster`] on the device.

#[allow(unused_imports)]
use crate::math::F32Ext;

pub const COLS: usize = 8;
pub const ROWS: usize = 3;
pub const CLOCKS: usize = COLS * ROWS;
/// LEDs per clock; the grid is 64 x 24, centred in 64 x 32.
pub const CELL: f32 = 8.0;

/// Both hands at 7:30: how the original's clocks rest, and how this patch
/// rested until card 160. Also where every hand starts before the first dance.
pub const REST: f32 = 225.0;
/// Degrees clockwise from 12 o'clock.
pub type Hands = [f32; 2];

/// How the dials that are not part of a digit are posed and drawn.
///
/// They must stay clock hands - visible, in an unobtrusive rest position (the
/// owner, 2026-09-19) - without reading as a glyph. Three of them stacked in a
/// column is the hard case: that is what the digit `1` leaves beside its bar,
/// and at 8 LEDs a repeated short stroke there is punctuation. `1` is also the
/// commonest digit on a clock face, so the wrong rest pose is on the panel more
/// often than not.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rest {
    /// What it is called, for the studio's "now playing".
    pub name: &'static str,
    /// Hour and minute hand, degrees clockwise from 12 o'clock.
    pub hands: Hands,
    /// Odd rows mirror the pose about the vertical, so the three resting dials
    /// beside a `1` never repeat one mark down a column.
    pub zigzag: bool,
    /// Ink, and hand length as a fraction of a digit hand's, while the time is
    /// being held. Full (`1.0`) during the dances: the rest pose only has to be
    /// unobtrusive when there is a time to read.
    pub ink: f32,
    /// Hand length as a share of full, while held.
    pub reach: f32,
}

const fn rest(name: &'static str, hands: Hands, zigzag: bool, ink: f32, reach: f32) -> Rest {
    Rest { name, hands, zigzag, ink, reach }
}

/// The minute hand's rest angle in the treatments that draw a diagonal: 1:30,
/// opposite the hour hand's 7:30. The dial reads about 1:37 and its two hands
/// make one stroke corner to corner across the cell.
///
/// A diagonal is the one direction the digits do not use - every digit stroke
/// is up, down, left or right - so a resting dial cannot be mistaken for part
/// of one, and it survives being looked at from across the room: three blurred
/// dots in a column are a colon, three blurred slashes are a texture.
const HATCH: f32 = 45.0;

/// The treatments, switchable live (the `rest` parameter) so they can be
/// judged on the panel rather than in a preview. `0` is what the patch did
/// before card 160; `DEFAULT_REST` is the default: card 160 recommended
/// `"hatched, quiet"`, and the owner chose `"hatched, faint"` on the panel.
pub const RESTS: &[Rest] = &[
    rest("as it was", [REST, REST], false, 1.0, 1.0),
    rest("quiet", [REST, REST], false, 0.20, 1.0),
    rest("hatched, quiet", [REST, HATCH], false, 0.20, 1.0),
    rest("hatched, faint", [REST, HATCH], false, 0.10, 0.9),
    rest("zigzag, quiet", [REST, REST], true, 0.20, 1.0),
];
pub const DEFAULT_REST: usize = 3;

impl Rest {
    /// The treatment a `rest` parameter value asks for.
    pub fn of(v: f32) -> Rest {
        RESTS[(v.max(0.0) as usize).min(RESTS.len() - 1)]
    }

    /// The pose for a resting dial in `row`.
    pub fn hands(&self, row: usize) -> Hands {
        match self.zigzag && row % 2 == 1 {
            true => self.hands.map(|a| (360.0 - a).rem_euclid(360.0)),
            false => self.hands,
        }
    }

    /// `[ink, length]` for `draw::Dials`, `settled` of the way from a dancing
    /// dial (full) to a resting one.
    ///
    /// Eased, and the ink moves in lightness rather than in light. Light going
    /// down in even steps looks like nothing much and then a sudden drop, since
    /// the eye goes as the cube root; even steps of lightness look even.
    pub fn scale(&self, settled: f32) -> [f32; 2] {
        let t = settled.clamp(0.0, 1.0);
        let t = t * t * (3.0 - 2.0 * t);
        let lightness = 1.0 + (self.ink.powf(1.0 / 3.0) - 1.0) * t;
        [lightness * lightness * lightness, 1.0 + (self.reach - 1.0) * t]
    }
}

/// Each digit is 2 clocks wide and 3 tall, read in rows. U R D L are hand
/// directions and N is the rest pose. Where a junction would need three hands,
/// something has to give. 0, 2, 5, 6 and 9 were checked against footage of
/// the original (its 9 keeps the vertical and lets the bar fall short); the
/// rest follow manu.ninja's table, drawn from the studio's promotional films,
/// including the "cyclops" 8: a closed box over a cup.
pub const DIGITS: [[&str; 6]; 10] = [
    ["RD", "LD", "UD", "UD", "UR", "UL"],
    ["NN", "DD", "NN", "UD", "NN", "UU"],
    ["RR", "LD", "RD", "LU", "UR", "LL"],
    ["RR", "LD", "RR", "LU", "RR", "LU"],
    ["DD", "DD", "UR", "UD", "NN", "UU"],
    ["RD", "LL", "UR", "LD", "RR", "LU"],
    ["RD", "LL", "UD", "LD", "UR", "UL"],
    ["RR", "LD", "NN", "UD", "NN", "UU"],
    ["RD", "LD", "UR", "UL", "UR", "UL"],
    ["RD", "LD", "UR", "UD", "RR", "LU"],
];

pub fn direction(c: u8) -> f32 {
    match c {
        b'U' => 0.0,
        b'R' => 90.0,
        b'D' => 180.0,
        b'L' => 270.0,
        _ => REST,
    }
}

/// Hand angles for all 24 clocks showing `hh:mm`. Four digits, leading zeros,
/// no separator: `09:05` is `0905`. That is the original's format and the
/// owner's decision (card 160); a colon is not this patch's to add.
pub fn pose(hh: u32, mm: u32, rest: Rest) -> [Hands; CLOCKS] {
    let mut out = [[REST; 2]; CLOCKS];
    for (d, digit) in [hh / 10, hh % 10, mm / 10, mm % 10].into_iter().enumerate() {
        for (k, cell) in DIGITS[digit as usize % 10].iter().enumerate() {
            let (row, col) = (k / 2, d * 2 + k % 2);
            let (b, at_rest) = (cell.as_bytes(), rest.hands(row));
            out[row * COLS + col] = [0, 1].map(|h| match b[h] {
                b'N' => at_rest[h],
                c => direction(c),
            });
        }
    }
    out
}

/// Which of the 24 dials are not part of a digit at `hh:mm`, and so are drawn
/// as being at rest.
pub fn resting(hh: u32, mm: u32) -> [bool; CLOCKS] {
    let mut out = [false; CLOCKS];
    for (d, digit) in [hh / 10, hh % 10, mm / 10, mm % 10].into_iter().enumerate() {
        for (k, cell) in DIGITS[digit as usize % 10].iter().enumerate() {
            let (row, col) = (k / 2, d * 2 + k % 2);
            out[row * COLS + col] = cell.bytes().all(|b| b == b'N');
        }
    }
    out
}


/// A resting dial's held, dark ink (card 188, brief 2.1.1). Index 0, **the
/// default**, is `"as it was"`: the old continuous `tint.scale(ink)`,
/// untouched - the review after this card's first cut found the default
/// picture had moved (the warm, graded rest shade the author had already
/// tuned became a much dimmer, nearly flat one), which the card's own rule
/// forbids. Every other entry is a short, hand-picked list of level triples,
/// chosen **nearest the levels the old continuous colour actually measures
/// at** (see the Log for the numbers) rather than computed or guessed - the
/// same look, steadied, not a different one. ``level_triple``
/// (card 188 deliverable 2) is the snap that turns a triple into the exact
/// colour the panel shows.
pub const DARK_CHOICES: &[&str] = &["as it was", "aligned warm", "aligned neutral", "aligned dim"];
pub const DEFAULT_DARK: usize = 0;

/// One ramp per [`DARK_CHOICES`] entry *after* `"as it was"` - `DARK_RAMPS[0]`
/// is `DARK_CHOICES[1]`, and so on. Each is darkest-first (ink ~0.10,
/// `"hatched, faint"`, then ink ~0.20, the other three treatments), so
/// [`dark_shade`] can pick further in as a treatment's `ink` asks for more
/// light.
pub const DARK_RAMPS: &[&[[u32; 3]]] = &[
    // "aligned warm": the nearest aligned triple to what the old continuous
    // ink actually measured (card 188 log, 2026-09-21 - "measured, after the
    // author's review"): ink 0.10 -> levels (6, 5, 3); ink 0.20 -> (12, 10, 7).
    // Keeps the hour hand's own warm hue as far as per-channel alignment
    // allows - the graded look the author had tuned, steadied rather than
    // replaced.
    &[[6, 5, 3], [12, 10, 7]],
    // "aligned neutral": the same two rungs with the channels equalised
    // (their rounded average) - no hue, same overall brightness.
    &[[5, 5, 5], [10, 10, 10]],
    // "aligned dim": this card's first cut, kept as a fourth, deliberately
    // much darker choice now that it is not the default.
    &[[1, 1, 1], [2, 2, 2], [3, 3, 3]],
];

/// The `dark` parameter's pick, as a level triple: `None` for `"as it was"`,
/// and further into the ramp as `ink` calls for more light.
pub fn dark_levels(variant: usize, ink: f32) -> Option<[u32; 3]> {
    if variant == 0 {
        return None;
    }
    let ramp = DARK_RAMPS[(variant - 1).min(DARK_RAMPS.len() - 1)];
    let idx = ((ink * ramp.len() as f32).ceil() as usize).clamp(1, ramp.len()) - 1;
    Some(ramp[idx])
}
