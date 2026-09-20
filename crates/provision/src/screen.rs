//! What the panel shows while the device is being provisioned, drawn into an
//! ordinary [`Rgb888Frame`].
//!
//! Three screens, from research 007 section 9.2 and card 221:
//!
//! * [`Layout::QrAndName`] (007's layout A) - the version 2-L QR hard left
//!   with its 3-pixel lit quiet zone in columns 0..=30, and the 32 columns
//!   that are left carrying eight `FONT_4X6` characters per line.
//! * [`Layout::Text`] (007's layout C) - no QR at all: the fallback for a
//!   name a QR cannot carry. The two used to alternate every four seconds;
//!   a code that keeps leaving the panel does not scan, and layout A names
//!   the network for whoever joins by hand, so the machine no longer does.
//! * [`Screen::Connected`] - the acquired IP address after a successful trial
//!   join. The panel is the one channel that cannot be lost when the radio
//!   switches channel, and Chrome on Android does not resolve `.local`, so
//!   this is how the user finds the device afterwards.
//!
//! Nothing here allocates, floats or reads a clock. The frame is the
//! caller's: on the device it is the same triple buffer every other screen
//! draws into, so this module costs no RAM of its own beyond the QR encoder's
//! two 80-byte scratch buffers.
//!
//! Polarity is the measured one and **must not change**: the quiet zone and
//! the light modules are lit white, the dark modules are off. That is what
//! the owner's phone scanned on 2026-09-19.

use embedded_graphics::mono_font::ascii::{FONT_4X6, FONT_5X7};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};
use screeny_proto::{Rgb888Frame, H, NBYTES, W};

use crate::qr::{self, QR_MODULES};
use crate::uri::{self, UriForm};

/// The address the portal answers on. `edge-dhcp`'s default pool, the DNS
/// catch-all's answer and this screen all have to say the same thing.
pub const PORTAL_IP: &str = "192.168.4.1";

/// Lit quiet zone around the QR, in pixels. Measured, not chosen.
pub const QUIET: i32 = 3;

/// Left edge of the QR block, including its quiet zone.
const QR_X: i32 = QUIET;
/// Top-left module of the code itself.
const QR_Y: i32 = (H as i32 - QR_MODULES as i32) / 2;
/// First text column of layout A: 3 + 25 + 3 + 1.
const TEXT_X: i32 = QR_X + QR_MODULES as i32 + QUIET + 1;
/// Characters of `FONT_4X6` that fit between [`TEXT_X`] and the right edge.
pub const TEXT_COLS: usize = (W - TEXT_X as usize) / 4;

const _: () = assert!(TEXT_X == 32);
const _: () = assert!(TEXT_COLS == 8);

// The palette `firmware/src/screens.rs` already uses, so the provisioning
// screens and the idle screen look like the same device. Deliberately not
// white: this screen can be up for hours.
const TITLE: Rgb888 = Rgb888::new(0x5a, 0x9e, 0xff);
const LABEL: Rgb888 = Rgb888::new(0x70, 0x70, 0x70);
const VALUE: Rgb888 = Rgb888::new(0xc8, 0xc8, 0xc8);
const OK: Rgb888 = Rgb888::new(0x30, 0xc0, 0x50);

/// Which portal layout to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// 007's layout A: QR left, name and instruction right.
    QrAndName,
    /// 007's layout C: text only, for a phone that will not scan and for a
    /// name no version 2 code can carry.
    Text,
}

/// A screen this crate knows how to draw.
///
/// Borrowed rather than owned: the SSID lives in the state machine and the
/// firmware draws straight out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Screen<'a> {
    /// The captive portal is up. Join `ssid`, then open [`PORTAL_IP`].
    Portal {
        /// The soft-AP's name, always `screeny-<id>`.
        ssid: &'a str,
        /// The QR layout, or the text one for a name no QR can carry.
        layout: Layout,
        /// Which `WIFI:` spelling the QR carries.
        form: UriForm,
    },
    /// A trial join succeeded and the device is at `ip`. Shown for
    /// [`crate::CONNECTED_SCREEN_MS`] so the user can read it before the
    /// panel goes back to its normal business.
    Connected {
        /// The address DHCP handed us, as four octets.
        ip: [u8; 4],
    },
    /// A firmware image is being written to the inactive slot (card 240).
    ///
    /// **The one screen that outranks a stream.** `docs/design/device-web.md`
    /// decision 7 is that the frame path is the product and nothing may take
    /// the panel from a sender - "except a firmware update, which is allowed
    /// to take the panel over with an 'updating' screen", and this is it.
    ///
    /// It lives in this crate rather than in `firmware/src/screens.rs` for
    /// the reason every other screen here does: the simulator draws it too,
    /// and a screen with two implementations is a screen that looks different
    /// on the two devices somebody is comparing. It is static apart from the
    /// bar, and it is drawn with **dither off** (research 006 section 4), so
    /// core 1 sleeps and the circular DMA loops: a 50 ms flash stall then
    /// costs nothing at all.
    Updating {
        /// How far through, 0..=100, or `None` before the length is known.
        ///
        /// An upload's length comes from `Content-Length`, which a sender may
        /// not have sent; `None` draws the bar's outline and no fill rather
        /// than inventing a figure.
        percent: Option<u8>,
    },
}

/// Why a screen could not be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderError {
    /// [`Layout::QrAndName`] was asked for with an SSID no version 2-L code
    /// can carry. [`crate::Provisioner::screen`] never does this - it picks
    /// [`Layout::Text`] instead - so this is the backstop for a caller that
    /// built the [`Screen`] by hand.
    NoQr,
}

/// Draw `screen` into `frame`, clearing it first.
///
/// # Errors
///
/// [`RenderError::NoQr`] - see that variant.
pub fn render(screen: &Screen<'_>, frame: &mut Rgb888Frame) -> Result<(), RenderError> {
    frame.fill(0);
    let mut t = FrameTarget(frame);
    match *screen {
        Screen::Portal {
            ssid,
            layout: Layout::QrAndName,
            form,
        } => {
            let payload = uri::wifi_uri(form, ssid).map_err(|_| RenderError::NoQr)?;
            let code = qr::encode(&payload).map_err(|_| RenderError::NoQr)?;
            blit_qr(&mut t, &code);
            // Eight characters per line in the 32 columns beside the code.
            let title = MonoTextStyle::new(&FONT_4X6, TITLE);
            let label = MonoTextStyle::new(&FONT_4X6, LABEL);
            let value = MonoTextStyle::new(&FONT_4X6, VALUE);
            text(&mut t, "set up", TEXT_X, 2, title);
            text(&mut t, "join", TEXT_X, 10, label);
            let (head, tail) = split_at_cols(ssid, TEXT_COLS);
            text(&mut t, head, TEXT_X, 16, value);
            text(&mut t, tail, TEXT_X, 22, value);
        }
        Screen::Portal {
            ssid,
            layout: Layout::Text,
            ..
        } => {
            // The wording card 221 settled on: what to do, what it is called,
            // what to do next, where to go.
            let title = MonoTextStyle::new(&FONT_5X7, TITLE);
            let label = MonoTextStyle::new(&FONT_4X6, LABEL);
            let value = MonoTextStyle::new(&FONT_4X6, VALUE);
            text(&mut t, "join wifi", 1, 1, title);
            text(&mut t, cut(ssid, W / 4), 1, 10, value);
            text(&mut t, "then open", 1, 17, label);
            text(&mut t, PORTAL_IP, 1, 24, value);
        }
        Screen::Connected { ip } => {
            let title = MonoTextStyle::new(&FONT_5X7, OK);
            let label = MonoTextStyle::new(&FONT_4X6, LABEL);
            let value = MonoTextStyle::new(&FONT_4X6, VALUE);
            text(&mut t, "connected", 1, 3, title);
            text(&mut t, "find me at", 1, 14, label);
            let mut line: heapless::String<16> = heapless::String::new();
            let _ = core::fmt::write(
                &mut line,
                format_args!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
            );
            text(&mut t, &line, 1, 22, value);
        }
        Screen::Updating { percent } => {
            let title = MonoTextStyle::new(&FONT_5X7, TITLE);
            let label = MonoTextStyle::new(&FONT_4X6, LABEL);
            text(&mut t, "updating", 1, 2, title);
            text(&mut t, "do not unplug", 1, 12, label);
            progress_bar(&mut t, percent);
        }
    }
    Ok(())
}

/// The one moving thing on the updating screen: a 62x7 outline that fills
/// left to right.
///
/// An outline and not a bare fill, because `None` and 0% have to look
/// different: an upload whose length nobody declared shows an empty bar, and
/// an empty bar with no outline is a blank panel, which is what a *hung*
/// device looks like. Deliberately dim - it may be up for half a minute on a
/// USB-powered panel, and the brightness setting is applied to this frame like
/// any other.
fn progress_bar(t: &mut FrameTarget<'_>, percent: Option<u8>) {
    const X0: i32 = 1;
    const X1: i32 = W as i32 - 2;
    const Y0: i32 = 22;
    const Y1: i32 = 28;
    let edge = [0x40, 0x40, 0x40];
    for x in X0..=X1 {
        t.put(x, Y0, edge);
        t.put(x, Y1, edge);
    }
    for y in Y0..=Y1 {
        t.put(X0, y, edge);
        t.put(X1, y, edge);
    }
    let Some(p) = percent else { return };
    let inner = X1 - X0 - 1; // columns between the two edges
    let filled = (inner * i32::from(p.min(100))) / 100;
    for x in 0..filled {
        for y in Y0 + 2..=Y1 - 2 {
            t.put(X0 + 1 + x, y, [0x5a, 0x9e, 0xff]);
        }
    }
}

/// One LED per module, standard polarity, lit quiet zone. Do not change.
fn blit_qr(t: &mut FrameTarget<'_>, code: &qr::Qr) {
    let size = code.size() as i32;
    for y in -QUIET..size + QUIET {
        for x in -QUIET..size + QUIET {
            // A *set* module is dark; everything else, quiet zone included,
            // is lit.
            if !code.dark(x, y) {
                t.put(QR_X + x, QR_Y + y, [0xff, 0xff, 0xff]);
            }
        }
    }
}

fn text(t: &mut FrameTarget<'_>, s: &str, x: i32, y: i32, style: MonoTextStyle<'_, Rgb888>) {
    // `embedded-graphics`' Text::draw is infallible against this target.
    let _ = Text::with_baseline(s, Point::new(x, y), style, Baseline::Top).draw(t);
}

/// Split `s` into two lines of at most `n` characters, on a char boundary.
fn split_at_cols(s: &str, n: usize) -> (&str, &str) {
    match s.char_indices().nth(n) {
        Some((i, _)) => (&s[..i], cut(&s[i..], n)),
        None => (s, ""),
    }
}

/// First `n` characters of `s`, never splitting a character.
pub(crate) fn cut(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// An `embedded-graphics` draw target over the plain byte array the rest of
/// the system calls a frame. Bounds-checked, never panics, never allocates.
struct FrameTarget<'a>(&'a mut Rgb888Frame);

impl FrameTarget<'_> {
    fn put(&mut self, x: i32, y: i32, rgb: [u8; 3]) {
        if x < 0 || y < 0 || x as usize >= W || y as usize >= H {
            return;
        }
        let i = (y as usize * W + x as usize) * 3;
        debug_assert!(i + 3 <= NBYTES);
        self.0[i..i + 3].copy_from_slice(&rgb);
    }
}

impl OriginDimensions for FrameTarget<'_> {
    fn size(&self) -> Size {
        Size::new(W as u32, H as u32)
    }
}

impl DrawTarget for FrameTarget<'_> {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Self::Color>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            self.put(p.x, p.y, [c.r(), c.g(), c.b()]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(frame: &Rgb888Frame, x: usize, y: usize) -> bool {
        let i = (y * W + x) * 3;
        frame[i] != 0 || frame[i + 1] != 0 || frame[i + 2] != 0
    }

    #[test]
    fn layout_a_puts_the_quiet_zone_and_the_block_where_the_bench_measured_it() {
        let mut f = [0u8; NBYTES];
        render(
            &Screen::Portal {
                ssid: "screeny-4a00a4",
                layout: Layout::QrAndName,
                form: UriForm::NoPass,
            },
            &mut f,
        )
        .unwrap();
        // The lit block is 31x31 at columns 0..=30, rows 0..=30.
        for x in 0..31 {
            assert!(lit(&f, x, 0), "top quiet row, column {x}");
            assert!(lit(&f, x, 30), "bottom quiet row, column {x}");
        }
        // Column 31 is the one-pixel gutter before the text.
        for y in 0..H {
            assert!(!lit(&f, 31, y), "gutter column at row {y}");
        }
        // Row 31 is below the block and outside every glyph.
        for x in 0..31 {
            assert!(!lit(&f, x, 31), "row below the block, column {x}");
        }
        // Something is drawn in the text half.
        assert!((TEXT_X as usize..W).any(|x| (0..H).any(|y| lit(&f, x, y))));
    }

    #[test]
    fn the_corner_of_the_finder_pattern_is_dark() {
        let mut f = [0u8; NBYTES];
        render(
            &Screen::Portal {
                ssid: "screeny-4a00a4",
                layout: Layout::QrAndName,
                form: UriForm::NoPass,
            },
            &mut f,
        )
        .unwrap();
        // Module (1,1) of the top-left finder is light, (0,0) is dark.
        assert!(!lit(&f, QR_X as usize, QR_Y as usize));
        assert!(lit(&f, QR_X as usize + 1, QR_Y as usize + 1));
    }

    #[test]
    fn layout_a_refuses_a_name_no_version_2_code_can_carry() {
        let mut f = [0u8; NBYTES];
        assert_eq!(
            render(
                &Screen::Portal {
                    ssid: "screeny-a-very-long-name",
                    layout: Layout::QrAndName,
                    form: UriForm::NoPass,
                },
                &mut f,
            ),
            Err(RenderError::NoQr)
        );
    }

    #[test]
    fn layout_c_lights_no_more_than_text_does() {
        let mut f = [0u8; NBYTES];
        render(
            &Screen::Portal {
                ssid: "screeny-4a00a4",
                layout: Layout::Text,
                form: UriForm::NoPass,
            },
            &mut f,
        )
        .unwrap();
        let on = (0..W).flat_map(|x| (0..H).map(move |y| (x, y)));
        let n = on.filter(|&(x, y)| lit(&f, x, y)).count();
        assert!(n > 100, "the text screen drew nothing ({n} pixels)");
        assert!(n < 700, "the text screen is suspiciously bright ({n} pixels)");
    }

    #[test]
    fn the_connected_screen_spells_the_address_out() {
        let mut f = [0u8; NBYTES];
        render(&Screen::Connected { ip: [192, 168, 7, 221] }, &mut f).unwrap();
        assert!((0..W).any(|x| (16..H).any(|y| lit(&f, x, y))));
    }

    #[test]
    fn nothing_renders_a_full_white_frame() {
        // CLAUDE.md: the panel runs off laptop USB. The QR's lit area is
        // 31x31 of 64x32 and nothing else comes close.
        for s in [
            Screen::Portal {
                ssid: "screeny-4a00a4",
                layout: Layout::QrAndName,
                form: UriForm::NoPass,
            },
            Screen::Portal {
                ssid: "screeny-4a00a4",
                layout: Layout::Text,
                form: UriForm::NoPass,
            },
            Screen::Connected { ip: [192, 168, 7, 221] },
            Screen::Updating { percent: Some(100) },
            Screen::Updating { percent: None },
        ] {
            let mut f = [0u8; NBYTES];
            render(&s, &mut f).unwrap();
            let full = f.as_chunks::<3>().0.iter().filter(|p| *p == &[0xff, 0xff, 0xff]).count();
            assert!(
                full * 100 / (W * H) <= 50,
                "more than half the panel is full white"
            );
        }
    }

    /// Card 240. The bar has to say three things apart: "nothing yet"
    /// (outline only), "part way" and "done". A screen that looked the same at
    /// 0% and at 100% would be no use to somebody watching the panel to decide
    /// whether to unplug it.
    #[test]
    fn the_updating_bar_fills_from_nothing_to_the_full_width() {
        let filled = |percent| {
            let mut f = [0u8; NBYTES];
            render(&Screen::Updating { percent }, &mut f).unwrap();
            // Row 24 is inside the bar, between its two edges.
            (2..W - 2).filter(|x| lit(&f, *x, 24)).count()
        };
        let none = filled(None);
        let zero = filled(Some(0));
        let half = filled(Some(50));
        let all = filled(Some(100));
        assert_eq!(none, 0, "an unknown length draws the outline and no fill");
        assert_eq!(zero, 0);
        assert!(half > 25 && half < 35, "half way is about half the bar ({half})");
        assert!(all >= 58, "a finished upload fills the bar ({all})");
        // Out of range is clamped, not wrapped.
        assert_eq!(filled(Some(200)), all);
    }

    /// The outline is there even when there is nothing to fill it with, so
    /// "no Content-Length" and "the panel has died" do not look the same.
    #[test]
    fn the_updating_screen_is_never_blank() {
        let mut f = [0u8; NBYTES];
        render(&Screen::Updating { percent: None }, &mut f).unwrap();
        let n = (0..W).flat_map(|x| (0..H).map(move |y| (x, y)))
            .filter(|&(x, y)| lit(&f, x, y))
            .count();
        assert!(n > 100, "the updating screen drew almost nothing ({n} pixels)");
        assert!(n < 700, "the updating screen is suspiciously bright ({n} pixels)");
    }

    #[test]
    fn splitting_the_name_never_splits_a_character() {
        assert_eq!(split_at_cols("screeny-4a00a4", 8), ("screeny-", "4a00a4"));
        assert_eq!(split_at_cols("short", 8), ("short", ""));
        // Four 3-byte characters: the split is by character, not by byte.
        assert_eq!(
            split_at_cols("\u{4e00}\u{4e01}\u{4e02}", 2),
            ("\u{4e00}\u{4e01}", "\u{4e02}")
        );
    }

    #[test]
    fn a_long_name_on_the_text_screen_is_cut_not_wrapped() {
        // 16 characters of `FONT_4X6` is the full 64-pixel width. A longer
        // name is trimmed to that, not wrapped onto the line below - which
        // is the line that says where to point a browser.
        let long = "screeny-an-extremely-long-name-indeed";
        let mut a = [0u8; NBYTES];
        let mut b = [0u8; NBYTES];
        render(
            &Screen::Portal { ssid: long, layout: Layout::Text, form: UriForm::NoPass },
            &mut a,
        )
        .unwrap();
        render(
            &Screen::Portal { ssid: &long[..16], layout: Layout::Text, form: UriForm::NoPass },
            &mut b,
        )
        .unwrap();
        assert_eq!(a, b);
    }
}
