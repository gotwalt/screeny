//! Regenerate the portal-screen pictures under `docs/research/img/221-*.png`
//! and print each one as ASCII art.
//!
//! ```text
//! cargo run -p screeny-provision --example portal-png
//! ```
//!
//! The PNGs are committed artefacts: `tests/render.rs` pins a hash of the
//! frame bytes, so when a layout changes on purpose the loop is run this,
//! look at the pictures, then paste the new hashes in. Nearest-neighbour 8x,
//! because a 64x32 PNG is unreadable in a browser.
//!
//! This is the successor to `lab/src/bin/portal-mock.rs`, which explored the
//! three candidate layouts for card 201. The difference is that the mock drew
//! its own picture; this one asks `screeny-provision` for the same frame the
//! firmware will put on the panel, so the layout no longer exists twice.

use screeny_provision::{render, Layout, Screen, UriForm};
use screeny_proto::{H, NBYTES, W};

const SCALE: usize = 8;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/research/img")
            .to_string_lossy()
            .into_owned()
    });
    let out = std::path::Path::new(&out);
    std::fs::create_dir_all(out).expect("mkdir");

    let ssid = "screeny-4a00a4";
    let screens: [(&str, Screen); 3] = [
        (
            "221-portal-a-qr-and-name",
            Screen::Portal {
                ssid,
                layout: Layout::QrAndName,
                form: UriForm::NoPass,
            },
        ),
        (
            "221-portal-c-text-only",
            Screen::Portal {
                ssid,
                layout: Layout::Text,
                form: UriForm::NoPass,
            },
        ),
        (
            "221-connected",
            Screen::Connected {
                ip: [192, 168, 7, 221],
            },
        ),
    ];

    for (name, screen) in screens {
        let mut f = [0u8; NBYTES];
        render(&screen, &mut f).expect("render");
        let path = out.join(format!("{name}.png"));
        write_png(&path, &f);
        println!("\n== {name} -> {} ({})", path.display(), fnv1a(&f));
        print!("{}", ascii(&f));
    }
}

fn write_png(path: &std::path::Path, f: &[u8; NBYTES]) {
    let (w, h) = (W * SCALE, H * SCALE);
    let mut buf = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let i = ((y / SCALE) * W + x / SCALE) * 3;
            let j = (y * w + x) * 3;
            buf[j..j + 3].copy_from_slice(&f[i..i + 3]);
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

fn ascii(f: &[u8; NBYTES]) -> String {
    let mut s = String::new();
    let rule = |s: &mut String| {
        s.push('+');
        for _ in 0..W {
            s.push('-');
        }
        s.push_str("+\n");
    };
    rule(&mut s);
    for y in 0..H {
        s.push('|');
        for x in 0..W {
            let i = (y * W + x) * 3;
            let v = (f[i] as u16 + f[i + 1] as u16 + f[i + 2] as u16) / 3;
            s.push(match v {
                0..=15 => ' ',
                16..=95 => '.',
                96..=191 => '+',
                _ => '#',
            });
        }
        s.push_str("|\n");
    }
    rule(&mut s);
    s
}

fn fnv1a(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("0x{:04x}_{:04x}_{:04x}_{:04x}", h >> 48, (h >> 32) & 0xffff, (h >> 16) & 0xffff, h & 0xffff)
}
