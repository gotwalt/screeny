//! Built-in test patterns.
//!
//! These are the firmware's own eyes on the panel: every one of them exists
//! to make one specific defect impossible to miss on a camera still. They are
//! selectable at run time (see `testcmd`) so that a whole afternoon of
//! bring-up does not turn into an afternoon of reflashing.
//!
//! They stay in the shipped firmware. `pattern` is also a `screeny`
//! sub-command in `docs/design/architecture.md`, and a device that can draw
//! its own test card is a device you can debug without a sender.

use crate::display::Frame;
use crate::{COLS, ROWS};

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Pattern {
    /// Everything off. The reference for "is that glow real?".
    Black = 0,
    /// Origin, axis direction and mirroring, all at once. See [`orientation`].
    Orientation = 1,
    /// The spike's card: red, green, blue bands, each ramping left to right.
    Bands = 2,
    /// Isolated blocks on black, with clear space below each. Ghosting shows
    /// up as a faint copy one or two rows under a block.
    Ghost = 3,
    /// Full sRGB grey ramp, 0 at the left to 255 at the right, same on every
    /// row. Gamma is right when this looks like an even gradient.
    GreyRamp = 4,
    /// Sixteen flat grey steps of sRGB 0, 17, 34 ... 255. The acceptance test
    /// for brightness: every step must still be separable when dimmed.
    GreySteps = 5,
    /// sRGB 0..63 across the panel — the bottom quarter of the ramp, blown up.
    /// Undithered this is mostly black; it is the evidence for card 030.
    DarkRamp = 6,
    /// Flat sRGB 128 grey. Mid-grey must not read as white.
    MidGrey = 7,
    /// One lit row on black. The pattern that actually measures ghosting; see
    /// [`ghost_row`] and card 007's log.
    GhostRow = 8,
}

impl Pattern {
    pub fn from_u8(v: u8) -> Option<Pattern> {
        Some(match v {
            0 => Pattern::Black,
            1 => Pattern::Orientation,
            2 => Pattern::Bands,
            3 => Pattern::Ghost,
            4 => Pattern::GreyRamp,
            5 => Pattern::GreySteps,
            6 => Pattern::DarkRamp,
            7 => Pattern::MidGrey,
            8 => Pattern::GhostRow,
            _ => return None,
        })
    }
}

pub fn draw(frame: &mut Frame, pattern: Pattern) {
    frame.clear();
    match pattern {
        Pattern::Black => {}
        Pattern::Orientation => orientation(frame),
        Pattern::Bands => bands(frame),
        Pattern::Ghost => ghost(frame),
        Pattern::GreyRamp => grey_ramp(frame),
        Pattern::GreySteps => grey_steps(frame),
        Pattern::DarkRamp => dark_ramp(frame),
        Pattern::MidGrey => fill(frame, [128, 128, 128]),
        Pattern::GhostRow => ghost_row(frame),
    }
}

fn fill(frame: &mut Frame, rgb: [u8; 3]) {
    for px in frame.px.chunks_exact_mut(3) {
        px.copy_from_slice(&rgb);
    }
}

fn rect(frame: &mut Frame, x0: usize, y0: usize, w: usize, h: usize, rgb: [u8; 3]) {
    for y in y0..(y0 + h).min(ROWS) {
        for x in x0..(x0 + w).min(COLS) {
            frame.set(x, y, rgb);
        }
    }
}

/// A big "F", 11x17. Chosen because it is the letter that survives no
/// transformation: rotate it, mirror it in either axis, and it is a different
/// shape every time.
const F_GLYPH: [u16; 17] = [
    0x7FF, 0x7FF, 0x7FF, // top bar, 3 rows of 11
    0x007, 0x007, 0x007, 0x007, 0x007, // stem
    0x0FF, 0x0FF, 0x0FF, // middle bar, 8 wide
    0x007, 0x007, 0x007, 0x007, 0x007, 0x007, // stem
];

/// Origin, axis direction and mirroring in one frame.
///
/// * `(0,0)` alone is white — the origin.
/// * A **red** run of 8 pixels along the top edge, starting at the origin:
///   that is +x.
/// * A **green** run of 4 pixels down the left edge: that is +y, and it is
///   deliberately a different length from the red one so a 90-degree rotation
///   cannot be mistaken for the right answer.
/// * A 3x3 **yellow** block in the top-right corner and a single **blue**
///   pixel in the bottom-right: all four corners are distinguishable.
/// * A large **F** in the middle: catches mirroring, which corner markers
///   alone cannot.
fn orientation(frame: &mut Frame) {
    let white = [200, 200, 200];
    for x in 0..8 {
        frame.set(x, 0, [200, 0, 0]);
    }
    for y in 0..4 {
        frame.set(0, y, [0, 200, 0]);
    }
    frame.set(0, 0, white);
    rect(frame, COLS - 3, 0, 3, 3, [200, 200, 0]);
    frame.set(COLS - 1, ROWS - 1, [0, 80, 255]);

    let (ox, oy) = (26, 7);
    for (dy, bits) in F_GLYPH.iter().enumerate() {
        for dx in 0..11 {
            if bits & (1 << dx) != 0 {
                frame.set(ox + dx, oy + dy, white);
            }
        }
    }
}

fn bands(frame: &mut Frame) {
    for y in 0..ROWS {
        let band = match y * 3 / ROWS {
            0 => [255u16, 0, 0],
            1 => [0, 255, 0],
            _ => [0, 0, 255],
        };
        for x in 0..COLS {
            let k = (x * 255 / (COLS - 1)) as u16;
            frame.set(
                x,
                y,
                [
                    (band[0] * k / 255) as u8,
                    (band[1] * k / 255) as u8,
                    (band[2] * k / 255) as u8,
                ],
            );
        }
    }
}

/// The row that measures ghosting: one lit row, black everywhere else.
///
/// [`ghost`] is the pattern you look at; this is the one you measure, and card
/// 007 needed it because the camera bleeds a lit LED several rows into its
/// neighbours and a block pattern cannot tell that bloom from a ghost.
///
/// Ghosting is **one-sided** — a faint copy appears below a lit row, never
/// above — while bloom, lens flare and any residual grid misalignment are
/// symmetric. So the measurement is the row above against the row below, at
/// the same distance, which needs no exposure calibration. Card 007 got 1.00
/// against a method that moves 2.3x for a 6% ghost.
///
/// Row 8 specifically: 1/16 scan drives panel rows `y` and `y + 16` together,
/// so lighting row 8 in the upper half leaves rows 24 and 25 as a control for
/// leakage across the halves, and rows 0-7 and 10-15 as clean background.
fn ghost_row(frame: &mut Frame) {
    for x in 0..COLS {
        frame.set(x, 8, [255, 255, 255]);
    }
}

/// Three lit blocks with four blank rows under each. Ghosting on this panel
/// would be a faint copy of a lit row appearing one or two rows below it, so
/// the blank rows are the measurement and the colours say which channel leaks.
///
/// Note when reading it that 1/16 scan couples `y` with `y + 16`: the copy of
/// the top-left block lands on row 22, where the tall white block already is.
/// Use [`ghost_row`] for anything quantitative.
fn ghost(frame: &mut Frame) {
    rect(frame, 4, 2, 24, 4, [255, 255, 255]);
    rect(frame, 34, 2, 24, 4, [255, 0, 0]);
    rect(frame, 4, 12, 24, 4, [0, 255, 0]);
    rect(frame, 34, 12, 24, 4, [0, 0, 255]);
    // A tall block: vertical edges show column-direction smear, which is a
    // different defect from row ghosting and is easy to confuse with it.
    rect(frame, 20, 22, 8, 8, [255, 255, 255]);
}

fn grey_ramp(frame: &mut Frame) {
    for x in 0..COLS {
        let v = (x * 255 / (COLS - 1)) as u8;
        for y in 0..ROWS {
            frame.set(x, y, [v, v, v]);
        }
    }
}

fn grey_steps(frame: &mut Frame) {
    for x in 0..COLS {
        let v = ((x / 4) * 17) as u8;
        for y in 0..ROWS {
            frame.set(x, y, [v, v, v]);
        }
    }
}

fn dark_ramp(frame: &mut Frame) {
    for x in 0..COLS {
        let v = x as u8;
        for y in 0..ROWS {
            frame.set(x, y, [v, v, v]);
        }
    }
}
