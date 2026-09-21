//! The display half of the firmware: the sRGB888 frame, the conversion into
//! the DMA framebuffer, brightness, and temporal dithering.
//!
//! Orientation is fixed here and nowhere else: [`Frame`] is row-major with
//! the origin at the top-left, x increasing to the right and y downwards, and
//! `Frame::px[y * W + x]` lands on panel pixel `(x, y)` with no mirroring.
//! Card 007 proved that on the panel with an asymmetric pattern; see its log.

use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, RgbColor, Size};

use crate::gamma::{self, FRAC_BITS};
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

/// What we come up at when flash holds no brightness. 96/255 is 9 slots, about
/// 14% duty — a little above Tidbyt's shipping default of 12%, comfortable in a
/// lit room, and below the point where the bench camera clips a white pixel.
///
/// An **alias**, not a second copy of the number: `screeny-settings` has to know
/// the default too (a blank partition returns it from `Store::load`), and
/// `crates/settings/README.md` asked card 212 to make this the one definition.
/// The store's copy is the definition; this is the name the display code uses.
pub const DEFAULT_BRIGHTNESS: u8 = screeny_settings::DEFAULT_BRIGHTNESS;

// ---------------------------------------------------------------------------
// sRGB frame -> DMA framebuffer
// ---------------------------------------------------------------------------

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
/// Card 248: the four thresholds a row can use, hoisted out of the pixel
/// loop. `screeny_dither::threshold` reverses the bits of the phase counter,
/// which is a loop, and `phase` does not change within a frame — so it runs
/// four times per frame rather than 6,144 times. The per-pixel work left is
/// one `XOR`, which is less than the add-and-mask it replaces.
#[inline(always)]
fn row_thresholds(phase: u16, y: usize) -> [u16; 4] {
    let mut t = [0u16; 4];
    let mut x = 0;
    while x < 4 {
        t[x] = screeny_dither::threshold(phase, screeny_dither::bayer(x, y), FRAC_BITS);
        x += 1;
    }
    t
}

pub fn render(frame: &Frame, fb: &mut FrameBuffer, mode: Mode, phase: u16) {
    let mut row = [[0u8; 3]; COLS];
    for y in 0..ROWS {
        let src = &frame.px[y * COLS * 3..(y + 1) * COLS * 3];
        let th = row_thresholds(phase, y);
        for x in 0..COLS {
            let (r, g, b) = (src[x * 3], src[x * 3 + 1], src[x * 3 + 2]);
            let (qr, qg, qb) = if mode.gamma {
                (gamma::gamma_q(r), gamma::gamma_q(g), gamma::gamma_q(b))
            } else {
                (gamma::linear_q(r), gamma::linear_q(g), gamma::linear_q(b))
            };
            row[x] = if mode.dither {
                let t = th[x & 3];
                [
                    screeny_dither::quantise_dither(qr, t, FRAC_BITS),
                    screeny_dither::quantise_dither(qg, t, FRAC_BITS),
                    screeny_dither::quantise_dither(qb, t, FRAC_BITS),
                ]
            } else {
                [
                    screeny_dither::quantise_plain(qr, FRAC_BITS),
                    screeny_dither::quantise_plain(qg, FRAC_BITS),
                    screeny_dither::quantise_plain(qb, FRAC_BITS),
                ]
            };
        }
        fb.write_row(y, &row);
    }
}
