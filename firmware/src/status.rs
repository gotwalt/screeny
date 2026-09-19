//! The status screen: what the panel shows when nobody is streaming to it.
//!
//! It is drawn into the sRGB [`Frame`] like any other content, not straight
//! into the DMA buffer, so it goes through the same gamma and brightness path
//! as a streamed frame — a status screen that ignored the brightness setting
//! would be the one thing on the device that could dazzle you.
//!
//! Fonts are `embedded-graphics`' own `FONT_5X7` and `FONT_4X6`. Both are
//! ASCII-only bitmaps with zero character spacing, so `FONT_4X6` fits 16
//! characters across the panel — enough for `255.255.255.255` with room over.

use core::fmt::Write;

use embedded_graphics::mono_font::ascii::{FONT_4X6, FONT_5X7};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};

use crate::display::Frame;

/// What the `status` task knows about the network, in the order it learns it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Net {
    Joining,
    Associated,
    Address([u8; 4]),
    Lost,
}

/// Deliberately not white. The panel is bright, this screen may be up for
/// hours, and a blue-white on black matches what the stock firmware does.
const TITLE: Rgb888 = Rgb888::new(0x5a, 0x9e, 0xff);
const LABEL: Rgb888 = Rgb888::new(0x70, 0x70, 0x70);
const VALUE: Rgb888 = Rgb888::new(0xc8, 0xc8, 0xc8);
const OK: Rgb888 = Rgb888::new(0x30, 0xc0, 0x50);
const WARN: Rgb888 = Rgb888::new(0xd0, 0x80, 0x20);

pub fn draw(frame: &mut Frame, net: Net, brightness: u8) {
    frame.clear();

    let title = MonoTextStyle::new(&FONT_5X7, TITLE);
    let label = MonoTextStyle::new(&FONT_4X6, LABEL);
    let value = MonoTextStyle::new(&FONT_4X6, VALUE);

    // `Baseline::Top` so y is the top of the glyph box and the layout below
    // is just "rows", which is how one reasons about a 32-row panel.
    let _ = Text::with_baseline("screeny", Point::new(1, 0), title, Baseline::Top).draw(frame);

    let (state_text, state_colour): (&str, Rgb888) = match net {
        Net::Joining => ("joining wifi", WARN),
        Net::Associated => ("dhcp...", WARN),
        Net::Address(_) => ("ready", OK),
        Net::Lost => ("wifi lost", WARN),
    };
    let state = MonoTextStyle::new(&FONT_4X6, state_colour);
    let _ = Text::with_baseline(state_text, Point::new(1, 9), state, Baseline::Top).draw(frame);

    if let Net::Address(ip) = net {
        let mut line = heapless::String::<16>::new();
        let _ = write!(line, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        let _ = Text::with_baseline(&line, Point::new(1, 16), value, Baseline::Top).draw(frame);
        let _ =
            Text::with_baseline("screeny.local", Point::new(1, 23), label, Baseline::Top).draw(frame);
    } else {
        let mut line = heapless::String::<16>::new();
        let _ = write!(line, "bright {}", brightness);
        let _ = Text::with_baseline(&line, Point::new(1, 16), label, Baseline::Top).draw(frame);
    }
}
