//! Card 201: what the portal screen looks like on a 64x32 panel.
//!
//! Renders the candidate layouts at 1:1 and writes a PNG (8x nearest-neighbour
//! so it is legible in a browser) plus ASCII art to stdout. Uses the *same*
//! QR encoder the firmware spike links (`qrcodegen-no-heap`) and the same
//! `embedded-graphics` fonts `firmware/src/screens.rs` already draws with, so
//! the picture is the picture.
//!
//! ```
//! cargo run --release --bin portal-mock -- ../docs/research/img
//! ```
//!
//! The owner measured on 2026-09-19 that a version 2-L code
//! (`WIFI:T:nopass;S:screeny-4a00a4;;`, 32 bytes, 25x25 modules) at one LED
//! per module with a 3-pixel lit quiet zone scans easily from a phone. These
//! layouts are all built around that.

use embedded_graphics::mono_font::ascii::{FONT_4X6, FONT_5X7};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

const COLS: usize = 64;
const ROWS: usize = 32;

/// A 64x32 sRGB888 frame, laid out exactly like `firmware/src/display.rs`.
struct Frame {
    px: Vec<u8>,
}

impl Frame {
    fn new() -> Self {
        Self {
            px: vec![0; COLS * ROWS * 3],
        }
    }

    fn set(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        if x < COLS && y < ROWS {
            let i = (y * COLS + x) * 3;
            self.px[i..i + 3].copy_from_slice(&rgb);
        }
    }

    fn get(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * COLS + x) * 3;
        [self.px[i], self.px[i + 1], self.px[i + 2]]
    }
}

impl OriginDimensions for Frame {
    fn size(&self) -> Size {
        Size::new(COLS as u32, ROWS as u32)
    }
}

impl DrawTarget for Frame {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Self::Color>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 {
                self.set(p.x as usize, p.y as usize, [c.r(), c.g(), c.b()]);
            }
        }
        Ok(())
    }
}

/// The WIFI: URI of the ZXing "barcode contents" convention, which iOS Camera
/// (since iOS 11) and Android (since 10) both parse. `T:nopass` is an open
/// network and the password field is then omitted entirely.
fn wifi_uri(ssid: &str) -> String {
    // `\ ; , : "` must be backslash-escaped, per the ZXing convention that
    // Android's own `WifiQrCode.java` implements. The normative Wi-Fi Alliance
    // WPA3 spec v3.5 section 7.1 percent-encodes instead and Android does not
    // understand that, so an SSID outside `A-Za-z0-9-` has no portable
    // encoding at all. Ours never is.
    format!("WIFI:T:nopass;S:{};;", escape(ssid))
}

/// The two-spec-portable form for an **open** network: the WFA spec omits `T`
/// entirely for an unauthenticated network, and ZXing's parser defaults an
/// absent `T` to `nopass`. One string satisfies both, and it is nine bytes
/// shorter - which buys nine more characters of SSID inside the same 2-L code.
fn wifi_uri_open_short(ssid: &str) -> String {
    format!("WIFI:S:{};;", escape(ssid))
}

fn escape(ssid: &str) -> String {
    let mut esc = String::new();
    for c in ssid.chars() {
        if matches!(c, '\\' | ';' | ',' | ':' | '"') {
            esc.push('\\');
        }
        esc.push(c);
    }
    esc
}

struct Qr {
    size: usize,
    version: usize,
    modules: Vec<bool>,
}

fn encode(ssid: &str, max_version: u8) -> Option<Qr> {
    encode_payload(&wifi_uri(ssid), max_version)
}

fn encode_uri(uri: &str) -> Option<Qr> {
    encode_payload(uri, 4)
}

fn encode_payload(uri: &str, max_version: u8) -> Option<Qr> {
    let cap = Version::new(max_version).buffer_len();
    let mut tmp = vec![0u8; cap];
    let mut out = vec![0u8; cap];
    let qr = QrCode::encode_text(
        uri,
        &mut tmp,
        &mut out,
        QrCodeEcc::Low,
        Version::new(1),
        Version::new(max_version),
        None,
        false,
    )
    .ok()?;
    let size = qr.size() as usize;
    let mut modules = vec![false; size * size];
    for y in 0..size {
        for x in 0..size {
            modules[y * size + x] = qr.get_module(x as i32, y as i32);
        }
    }
    Some(Qr {
        size,
        version: (size - 17) / 4,
        modules,
    })
}

/// Blit the QR at `(ox, oy)`, one LED per module, with a lit quiet zone of
/// `quiet` pixels. Standard polarity: light modules and the quiet zone are
/// lit, dark modules are off. That is what scanned on the bench.
fn blit_qr(frame: &mut Frame, qr: &Qr, ox: i32, oy: i32, quiet: i32, level: u8) {
    let lit = [level, level, level];
    for y in -quiet..qr.size as i32 + quiet {
        for x in -quiet..qr.size as i32 + quiet {
            let dark = x >= 0
                && y >= 0
                && (x as usize) < qr.size
                && (y as usize) < qr.size
                && qr.modules[y as usize * qr.size + x as usize];
            if !dark {
                let (px, py) = (ox + x, oy + y);
                if px >= 0 && py >= 0 {
                    frame.set(px as usize, py as usize, lit);
                }
            }
        }
    }
}

const TITLE: Rgb888 = Rgb888::new(0x5a, 0x9e, 0xff);
const VALUE: Rgb888 = Rgb888::new(0xc8, 0xc8, 0xc8);
const LABEL: Rgb888 = Rgb888::new(0x70, 0x70, 0x70);

/// Layout A - the one to build. QR hard left with a 3-pixel quiet zone, text
/// in the 30 columns that are left.
fn layout_a(ssid: &str) -> Option<Frame> {
    let qr = encode(ssid, 2)?;
    let mut f = Frame::new();
    let quiet = 3;
    let ox = quiet;
    let oy = (ROWS as i32 - qr.size as i32) / 2;
    blit_qr(&mut f, &qr, ox, oy, quiet, 0xff);

    let tx = ox + qr.size as i32 + quiet + 1; // 3 + 25 + 3 + 1 = 32
    let s_title = MonoTextStyle::new(&FONT_4X6, TITLE);
    let s_val = MonoTextStyle::new(&FONT_4X6, VALUE);
    let s_lab = MonoTextStyle::new(&FONT_4X6, LABEL);
    // 64 - 32 = 32 columns = 8 characters of FONT_4X6.
    let _ = Text::with_baseline("set up", Point::new(tx, 2), s_title, Baseline::Top).draw(&mut f);
    let _ = Text::with_baseline("join", Point::new(tx, 10), s_lab, Baseline::Top).draw(&mut f);
    let _ = Text::with_baseline(&ssid[..ssid.len().min(8)], Point::new(tx, 16), s_val, Baseline::Top)
        .draw(&mut f);
    let _ = Text::with_baseline(&ssid[ssid.len().min(8)..], Point::new(tx, 22), s_val, Baseline::Top)
        .draw(&mut f);
    Some(f)
}

/// Layout B - QR only, centred, no text. The fallback when the SSID is long
/// enough to need version 3 (29x29 + a 1-pixel quiet zone = 31 of 32 rows).
fn layout_b(ssid: &str) -> Option<Frame> {
    let qr = encode(ssid, 3)?;
    let mut f = Frame::new();
    let quiet = if qr.size >= 29 { 1 } else { 3 };
    let ox = (COLS as i32 - qr.size as i32) / 2;
    let oy = (ROWS as i32 - qr.size as i32) / 2;
    blit_qr(&mut f, &qr, ox, oy, quiet, 0xff);
    Some(f)
}

/// Layout C - the "no QR" fallback, for a name a QR cannot carry at all, and
/// the screen a phone that will not scan still needs. Pure text.
fn layout_c(ssid: &str) -> Frame {
    let mut f = Frame::new();
    let s_title = MonoTextStyle::new(&FONT_5X7, TITLE);
    let s_val = MonoTextStyle::new(&FONT_4X6, VALUE);
    let s_lab = MonoTextStyle::new(&FONT_4X6, LABEL);
    let _ = Text::with_baseline("set up wifi", Point::new(1, 0), s_title, Baseline::Top).draw(&mut f);
    let _ = Text::with_baseline("join network", Point::new(1, 10), s_lab, Baseline::Top).draw(&mut f);
    let _ = Text::with_baseline(ssid, Point::new(1, 17), s_val, Baseline::Top).draw(&mut f);
    let _ = Text::with_baseline("192.168.4.1", Point::new(1, 24), s_lab, Baseline::Top).draw(&mut f);
    f
}

fn ascii(f: &Frame) -> String {
    let mut s = String::new();
    s.push('+');
    for _ in 0..COLS {
        s.push('-');
    }
    s.push_str("+\n");
    for y in 0..ROWS {
        s.push('|');
        for x in 0..COLS {
            let [r, g, b] = f.get(x, y);
            let v = (r as u16 + g as u16 + b as u16) / 3;
            s.push(match v {
                0..=15 => ' ',
                16..=95 => '.',
                96..=191 => '+',
                _ => '#',
            });
        }
        s.push_str("|\n");
    }
    s.push('+');
    for _ in 0..COLS {
        s.push('-');
    }
    s.push_str("+\n");
    s
}

fn write_png(path: &std::path::Path, f: &Frame, scale: usize) {
    let (w, h) = (COLS * scale, ROWS * scale);
    let mut buf = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let c = f.get(x / scale, y / scale);
            let i = (y * w + x) * 3;
            buf[i..i + 3].copy_from_slice(&c);
        }
    }
    let file = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(&buf)
        .expect("png data");
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "out".to_string());
    let out = std::path::Path::new(&out);
    std::fs::create_dir_all(out).expect("mkdir");

    let ssid = "screeny-4a00a4";
    let uri = wifi_uri(ssid);
    println!("payload {:?} = {} bytes", uri, uri.len());

    for max in [2u8, 3, 4] {
        if let Some(q) = encode(ssid, max) {
            println!(
                "  max version {max}: chose version {} ({}x{} modules)",
                q.version, q.size, q.size
            );
        }
    }
    let short = wifi_uri_open_short(ssid);
    println!("short open form {:?} = {} bytes", short, short.len());

    // Where the 32-byte version 2-L budget runs out, both ways.
    println!("  ssid   T:nopass form       short open form");
    for n in 6..=26usize {
        let name: String = core::iter::repeat('x').take(n).collect();
        let a = wifi_uri(&name);
        let b = wifi_uri_open_short(&name);
        let va = encode_uri(&a).map(|q| q.version).unwrap_or(0);
        let vb = encode_uri(&b).map(|q| q.version).unwrap_or(0);
        println!(
            "  {:2}     {:2} bytes -> v{}        {:2} bytes -> v{}",
            n,
            a.len(),
            va,
            b.len(),
            vb
        );
    }

    let mut frames: Vec<(&str, Frame)> = Vec::new();
    frames.push(("portal-a-qr-and-name", layout_a(ssid).expect("layout a")));
    frames.push((
        "portal-b-qr-only",
        layout_b("screeny-a-very-long-name").expect("layout b"),
    ));
    frames.push(("portal-c-text-only", layout_c(ssid)));

    for (name, f) in &frames {
        let png = out.join(format!("201-{name}.png"));
        write_png(&png, f, 8);
        println!("\n== {name} -> {}", png.display());
        print!("{}", ascii(f));
    }
}
