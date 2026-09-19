//! The display half of the firmware: the sRGB888 frame, the conversion into
//! the DMA framebuffer, brightness, and temporal dithering.
//!
//! Orientation is fixed here and nowhere else: [`Frame`] is row-major with
//! the origin at the top-left, x increasing to the right and y downwards, and
//! `Frame::px[y * W + x]` lands on panel pixel `(x, y)` with no mirroring.
//! Card 007 proved that on the panel with an asymmetric pattern; see its log.

use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, RgbColor, Size};

use crate::gamma::{self, FRAC, FRAC_BITS, LEVELS};
use crate::{FrameBuffer, COLS, ROWS};

pub const NPIX: usize = COLS * ROWS;

// ---------------------------------------------------------------------------
// The sRGB frame
// ---------------------------------------------------------------------------

/// One frame of sRGB888, in exactly the layout `crates/proto` decodes into.
///
/// `px` *is* [`screeny_proto::Rgb888Frame`] — a flat `[u8; 6144]`, three bytes
/// per pixel, row-major — so a decoder writes straight into the framebuffer
/// the display will scan out, with no repacking step in between.
#[derive(Clone)]
pub struct Frame {
    pub px: screeny_proto::Rgb888Frame,
}

impl Frame {
    pub const fn new() -> Self {
        Self {
            px: [0u8; NPIX * 3],
        }
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        if x < COLS && y < ROWS {
            let i = (y * COLS + x) * 3;
            self.px[i] = rgb[0];
            self.px[i + 1] = rgb[1];
            self.px[i + 2] = rgb[2];
        }
    }

    pub fn clear(&mut self) {
        self.px = [0u8; NPIX * 3];
    }

    /// Copy another frame over this one.
    pub fn copy_from(&mut self, other: &Frame) {
        self.px.copy_from_slice(&other.px);
    }

    /// `dst = self * n / 256`, per channel.
    pub fn scale(&mut self, n: u16) {
        let n = n.min(256) as u32;
        for b in self.px.iter_mut() {
            *b = ((*b as u32 * n + 128) >> 8) as u8;
        }
    }
}

/// So the status screen can use `embedded-graphics` text rendering and still
/// go through the same gamma and brightness path as a streamed frame.
impl OriginDimensions for Frame {
    fn size(&self) -> Size {
        Size::new(COLS as u32, ROWS as u32)
    }
}

impl DrawTarget for Frame {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>>,
    {
        for embedded_graphics::Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 {
                self.set(p.x as usize, p.y as usize, [c.r(), c.g(), c.b()]);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Brightness
// ---------------------------------------------------------------------------

/// Hard ceiling on output-enable duty, in pixel-clock slots per 64-slot scan
/// row. **This is the USB power budget and it is not a tuning knob.**
///
/// 25/64 is 39% duty, which is exactly what Tidbyt's own firmware calls
/// brightness 100 — their documented maximum (`hdk/src/display.h`). The
/// framebuffer could reach 55 slots (86%); we never ask it to.
pub const MAX_OE_SLOTS: usize = 25;

/// Runtime brightness 0..=255 -> output-enable slots.
///
/// Linear in duty, and therefore linear in emitted light. Note the real
/// resolution of the control is [`MAX_OE_SLOTS`] steps, not 256: the panel is
/// lit for a whole number of pixel clocks or not at all.
#[inline]
pub const fn slots_for(brightness: u8) -> usize {
    (brightness as usize * MAX_OE_SLOTS + 127) / 255
}

/// What we come up at. 96/255 is 9 slots, about 14% duty — a little above
/// Tidbyt's shipping default of 12%, comfortable in a lit room, and below the
/// point where the bench camera clips a white pixel.
pub const DEFAULT_BRIGHTNESS: u8 = 96;

// ---------------------------------------------------------------------------
// sRGB frame -> DMA framebuffer
// ---------------------------------------------------------------------------

/// 4x4 ordered (Bayer) matrix scaled to the 16 dither phases.
///
/// Used as a per-pixel *phase offset*, not as a spatial dither: without it
/// every pixel with the same remainder would toggle on the same refresh and
/// the whole panel would beat at `refresh/16` — about 10 Hz, which is very
/// visible. Offsetting by pixel spreads that across the panel so it averages
/// out spatially as well as temporally.
const BAYER4: [[u16; 4]; 4] = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// sRGB EOTF on the way to panel levels. Off is the spike's behaviour and
    /// exists only so the two can be photographed side by side.
    pub gamma: bool,
    /// Spend the sub-level remainder across successive panel refreshes.
    pub dither: bool,
}

impl Mode {
    pub const DEFAULT: Mode = Mode {
        gamma: true,
        dither: true,
    };
}

/// Converts one sRGB frame into `fb`, applying gamma and (optionally) the
/// dither phase. Returns nothing; the caller swaps.
///
/// Cost is measured in the card 007 log. The shape that matters: one pass,
/// row at a time, no `erase()` beforehand (every entry is written), and the
/// per-row `planes[p].rows[r]` lookup hoisted into `write_row`.
pub fn render(frame: &Frame, fb: &mut FrameBuffer, mode: Mode, phase: u16) {
    let mut row = [[0u8; 3]; COLS];
    for y in 0..ROWS {
        let src = &frame.px[y * COLS * 3..(y + 1) * COLS * 3];
        let bayer = &BAYER4[y & 3];
        for x in 0..COLS {
            let (r, g, b) = (src[x * 3], src[x * 3 + 1], src[x * 3 + 2]);
            let (qr, qg, qb) = if mode.gamma {
                (gamma::gamma_q(r), gamma::gamma_q(g), gamma::gamma_q(b))
            } else {
                (gamma::linear_q(r), gamma::linear_q(g), gamma::linear_q(b))
            };
            row[x] = if mode.dither {
                let t = (phase + bayer[x & 3]) & (FRAC - 1);
                [quantise_dither(qr, t), quantise_dither(qg, t), quantise_dither(qb, t)]
            } else {
                [(qr >> FRAC_BITS) as u8, (qg >> FRAC_BITS) as u8, (qb >> FRAC_BITS) as u8]
            };
        }
        fb.write_row(y, &row);
    }
}

/// `q` is duty in 1/16ths of a level. Emit the level below or above it so
/// that over a full phase cycle the mean is `q/16`.
#[inline(always)]
fn quantise_dither(q: u16, threshold: u16) -> u8 {
    let level = q >> FRAC_BITS;
    let frac = q & (FRAC - 1);
    let bumped = level + (frac > threshold) as u16;
    if bumped > LEVELS { LEVELS as u8 } else { bumped as u8 }
}
