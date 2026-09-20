//! What the panel shows while the device is being provisioned, drawn into an
//! ordinary [`Rgb888Frame`].
//!
//! Three screens, from research 007 section 9.2 and card 221:
//!
//! * [`Layout::QrAndName`] (007's layout A) - the version 2-L QR hard left
//!   with its 3-pixel lit quiet zone in columns 0..=30, and the 32 columns
//!   that are left carrying eight `FONT_4X6` characters per line.
//! * [`Layout::Text`] (007's layout C) - no QR at all. This is not only the
//!   fallback for a name a QR cannot carry; it is what a user whose phone
//!   will not scan needs, so the two alternate on a slow timer.
//! * [`Screen::Connected`] - the acquired IP address after a successful trial
//!   join. The panel is the one channel that cannot be lost when the radio
//!   switches channel, and Chrome on Android does not resolve `.local`, so
//!   this is how the user finds the device afterwards.
//!
//! Nothing here allocates, floats or reads a clock. The frame is the caller's
//! - on the device it is the same triple buffer every other screen draws
//! into, so this module costs no RAM of its own beyond the QR encoder's two
//! 80-byte scratch buffers.
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
        /// Which of the two alternating layouts this tick wants.
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
            text(&mut t, "join wifi", 1, 0, title);
            text(&mut t, cut(ssid, W / 4), 1, 9, value);
            text(&mut t, "then open", 1, 16, label);
            text(&mut t, PORTAL_IP, 1, 23, value);
        }
        Screen::Connected { ip } => {
            let title = MonoTextStyle::new(&FONT_5X7, OK);
            let label = MonoTextStyle::new(&FONT_4X6, LABEL);
            let value = MonoTextStyle::new(&FONT_4X6, VALUE);
            text(&mut t, "connected", 1, 0, title);
            text(&mut t, "find me at", 1, 11, label);
            let mut line: heapless::String<16> = heapless::String::new();
            let _ = core::fmt::write(
                &mut line,
                format_args!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
            );
            text(&mut t, &line, 1, 19, value);
        }
    }
    Ok(())
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
        ] {
            let mut f = [0u8; NBYTES];
            render(&s, &mut f).unwrap();
            let full = f.chunks_exact(3).filter(|p| p == &[0xff, 0xff, 0xff]).count();
            assert!(
                full * 100 / (W * H) <= 50,
                "more than half the panel is full white"
            );
        }
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
        let mut f = [0u8; NBYTES];
        render(
            &Screen::Portal {
                ssid: "screeny-an-extremely-long-name-indeed",
                layout: Layout::Text,
                form: UriForm::NoPass,
            },
            &mut f,
        )
        .unwrap();
        // 16 characters of FONT_4X6 is the full width; nothing spills past it
        // into the line below, because `cut` trimmed it.
        assert!(!lit(&f, 63, 14));
    }
}
