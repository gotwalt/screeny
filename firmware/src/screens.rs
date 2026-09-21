//! Everything the panel shows that did not arrive over the wire: the idle
//! status screen of spec section 7.5 and the `IDENTIFY` overlay of section 6.3.
//!
//! Both are drawn into the sRGB [`Frame`] like any other content, not straight
//! into the DMA buffer, so they go through the same gamma and brightness path
//! as a streamed frame — a status screen that ignored the brightness setting
//! would be the one thing on the device that could dazzle you.
//!
//! Fonts are `embedded-graphics`' own `FONT_5X7` and `FONT_4X6`. Both are
//! ASCII-only bitmaps with zero character spacing, so `FONT_4X6` fits 16
//! characters across the panel — enough for `255.255.255.255` with room over.
//! Card 007 photographed the 4x6 IP address and confirmed it is legible.

use core::fmt::Write;

use embedded_graphics::mono_font::ascii::{FONT_4X6, FONT_5X7};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};

use crate::display::Frame;
use crate::{COLS, ROWS};

/// What the frame task knows about the network, in the order it learns it.
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

/// The idle screen: name, address, signal, and a slow ambient sweep.
///
/// `phase` advances once per redraw (about 10 Hz); the sweep is the "slow
/// ambient animation" section 7.5 asks for, and it doubles as proof of life —
/// a frozen panel and an idle panel look identical without it.
pub fn status(frame: &mut Frame, name: &str, hostname: &str, net: Net, phase: u32) {
    frame.clear();

    let (state_text, state_colour): (&str, Rgb888) = match net {
        Net::Joining => ("joining wifi", WARN),
        Net::Associated => ("dhcp...", WARN),
        Net::Address(_) => ("ready", OK),
        Net::Lost => ("network down", WARN),
    };

    // Line 1 is the *friendly* name, which `SET_NAME` changes. A
    // 12-character name fits the wider font; anything longer drops to 4x6,
    // and past 15 characters it is cut rather than wrapped.
    if name.len() <= 12 {
        let s = MonoTextStyle::new(&FONT_5X7, TITLE);
        let _ = Text::with_baseline(name, Point::new(1, 0), s, Baseline::Top).draw(frame);
    } else {
        let s = MonoTextStyle::new(&FONT_4X6, TITLE);
        let _ = Text::with_baseline(cut(name, 15), Point::new(1, 1), s, Baseline::Top).draw(frame);
    }

    let state = MonoTextStyle::new(&FONT_4X6, state_colour);
    let _ = Text::with_baseline(state_text, Point::new(1, 9), state, Baseline::Top).draw(frame);
    // The bars share line 2 with the state text, which is at most twelve
    // characters wide; line 1 is the name, which can run the full width.
    rssi_bars(frame, crate::RSSI_DBM.load(core::sync::atomic::Ordering::Relaxed));

    let value = MonoTextStyle::new(&FONT_4X6, VALUE);
    let label = MonoTextStyle::new(&FONT_4X6, LABEL);
    if let Net::Address(ip) = net {
        let mut line = heapless::String::<16>::new();
        let _ = write!(line, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        let _ = Text::with_baseline(&line, Point::new(1, 16), value, Baseline::Top).draw(frame);
        // Line 4 is the *DNS* name, which `SET_NAME` does not change. The
        // two are the same by default and that is fine: one of them is what
        // you type and the other is what you called it.
        let mut host = heapless::String::<22>::new();
        let _ = host.push_str(hostname);
        if host.push_str(".local").is_err() || host.len() > 16 {
            host.clear();
            let _ = host.push_str(hostname);
        }
        let _ = Text::with_baseline(cut(&host, 16), Point::new(1, 23), label, Baseline::Top)
            .draw(frame);
    } else {
        let mut line = heapless::String::<16>::new();
        let _ = write!(
            line,
            "bright {}",
            crate::BRIGHTNESS.load(core::sync::atomic::Ordering::Relaxed)
        );
        let _ = Text::with_baseline(&line, Point::new(1, 16), label, Baseline::Top).draw(frame);
    }

    ambient(frame, phase);
}

/// Truncate to `n` characters, never splitting a UTF-8 code point: the name
/// comes from `SET_NAME` and is only promised to be valid UTF-8.
fn cut(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Four signal bars in the top-right corner, from the last beacon RSSI.
///
/// The thresholds are the usual Wi-Fi ones: -55 excellent, -65 good, -75
/// fair, below that one bar. An unfilled bar is drawn dim rather than left
/// black so that "no signal" and "no bars widget" cannot be confused.
fn rssi_bars(frame: &mut Frame, rssi_dbm: i8) {
    let bars = match rssi_dbm {
        0 => 0, // never reported
        r if r >= -55 => 4,
        r if r >= -65 => 3,
        r if r >= -75 => 2,
        _ => 1,
    };
    for b in 0..4usize {
        let h = b + 1;
        let x = COLS - 9 + b * 2;
        let c = if b < bars {
            [0x30, 0xc0, 0x50]
        } else {
            [0x18, 0x18, 0x18]
        };
        for dy in 0..h {
            frame.set(x, 13 - dy, c);
        }
    }
}

/// A single dim pixel crossing the bottom row, once every ~13 seconds.
fn ambient(frame: &mut Frame, phase: u32) {
    let x = (phase as usize / 2) % COLS;
    let y = ROWS - 1;
    for d in 0..6usize {
        if x < d {
            continue;
        }
        let fade = (6 - d) as u32;
        let v = (0x40 * fade / 6) as u8;
        frame.set(x - d, y, [v / 3, v / 2, v]);
    }
}

/// The crash-loop screen (card 243): the device has stopped on purpose.
///
/// Drawn once, at boot, before the network is brought up, by a device whose
/// breadcrumb says it has panicked [`crate::panic::CRASH_LOOP_MAX`] times in a
/// row. It is the whole user interface of that state - there is no HTTP, no
/// mDNS and no stream - so it says what happened, where, and what to do.
///
/// Deliberately dim and static: it may be up for days on a USB-powered panel,
/// and the one thing worse than a panel that has stopped is a panel that has
/// stopped *and* is flashing.
pub fn crashed(frame: &mut Frame, file: &str, line: u32, panics: u32) {
    frame.clear();
    let title = MonoTextStyle::new(&FONT_5X7, Rgb888::new(0xd0, 0x40, 0x30));
    let value = MonoTextStyle::new(&FONT_4X6, VALUE);
    let label = MonoTextStyle::new(&FONT_4X6, LABEL);
    let _ = Text::with_baseline("CRASHED", Point::new(1, 0), title, Baseline::Top).draw(frame);

    let mut where_ = heapless::String::<16>::new();
    // 16 characters: the longest base name this can hold is 12, and a line
    // number of four digits plus the colon is the rest. Written in that order
    // because the file is what identifies it and the line is the refinement.
    let _ = write!(where_, "{}", cut(file, 11));
    let _ = Text::with_baseline(&where_, Point::new(1, 9), value, Baseline::Top).draw(frame);
    let mut at = heapless::String::<16>::new();
    let _ = write!(at, "line {} x{}", line, panics);
    let _ = Text::with_baseline(&at, Point::new(1, 16), value, Baseline::Top).draw(frame);
    let _ = Text::with_baseline("power cycle", Point::new(1, 24), label, Baseline::Top).draw(frame);
}

/// The `IDENTIFY` overlay: "which one is this?".
///
/// Section 6.3 requires it to work in any state, including while another
/// sender holds the lock, so the frame task draws it over whatever else it
/// would have composed. High contrast, the name, and the address, because
/// those are the two things you are trying to match up.
///
/// **This is also the button's short press** (card 230): one screen, raised by
/// one mechanism, whether the request came over the wire or off the panel's
/// own button. Section 6.3 asks for "a high-contrast pattern plus the device
/// name and IP" and that is still what this is; card 230 adds the firmware
/// version and the signal on a third line, because the owner standing in front
/// of the panel with no laptop is the person the button is for.
pub fn identify(frame: &mut Frame, name: &str, net: Net, phase: u32) {
    // A moving chevron border: unmistakable across a room, and cheap.
    let on = (phase / 3) % 2 == 0;
    let (a, b) = if on {
        ([0xff, 0xff, 0x00], [0x00, 0x00, 0x00])
    } else {
        ([0x00, 0x00, 0x00], [0xff, 0xff, 0x00])
    };
    frame.clear();
    for x in 0..COLS {
        let c = if (x / 4) % 2 == 0 { a } else { b };
        frame.set(x, 0, c);
        frame.set(x, 1, c);
        frame.set(x, ROWS - 2, c);
        frame.set(x, ROWS - 1, c);
    }
    for y in 2..ROWS - 2 {
        let c = if (y / 4) % 2 == 0 { a } else { b };
        frame.set(0, y, c);
        frame.set(1, y, c);
        frame.set(COLS - 2, y, c);
        frame.set(COLS - 1, y, c);
    }

    let s = MonoTextStyle::new(&FONT_4X6, Rgb888::new(0xff, 0xff, 0xff));
    // Fourteen characters fit: the text starts at x=4, the 4x6 font is four
    // pixels wide, and the right-hand chevron border begins at x=62. Card 008
    // shipped 13 and said so.
    let _ = Text::with_baseline(cut(name, 14), Point::new(4, 8), s, Baseline::Top).draw(frame);
    if let Net::Address(ip) = net {
        let mut line = heapless::String::<16>::new();
        let _ = write!(line, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        let _ = Text::with_baseline(&line, Point::new(4, 15), s, Baseline::Top).draw(frame);
    }
    // Card 230's third line: the firmware version and the signal, because the
    // short press is also "what is this thing running?" and the panel is the
    // one place that answers without a network. Fourteen characters fit and
    // this is twelve; an unreported RSSI (0) prints the version alone rather
    // than a plausible-looking "0".
    let mut line = heapless::String::<16>::new();
    let rssi = crate::RSSI_DBM.load(core::sync::atomic::Ordering::Relaxed);
    let _ = if rssi == 0 {
        write!(line, "fw {}", crate::FW_VERSION)
    } else {
        write!(line, "fw {} {}", crate::FW_VERSION, rssi)
    };
    let _ = Text::with_baseline(&line, Point::new(4, 22), s, Baseline::Top).draw(frame);
}
