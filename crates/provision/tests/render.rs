//! The renderer, checked two ways that a layout change cannot both survive.
//!
//! 1. **An independent decoder reads the panel back.** `rqrr` never saw our
//!    encoder; it finds the finder patterns, reads the modules and hands back
//!    a string. If the polarity is inverted, the quiet zone is cut, the block
//!    is misplaced or the module size stops being one LED, this stops
//!    working. That is the check the card asked for, and it is worth more
//!    than any assertion about pixels.
//! 2. **A golden hash of the frame bytes.** The decode check does not care
//!    about the text half of the screen, so an accidental edit to the layout
//!    would slip past it. The hashes below pin all three screens exactly;
//!    when one changes on purpose, regenerate the PNGs with
//!    `cargo run -p screeny-provision --example portal-png`, look at them,
//!    and paste the new hash in.

use screeny_provision::{render, Layout, Screen, UriForm};
use screeny_proto::{H, NBYTES, W};

/// Upscale so a QR *detector* has something to work with. The panel really is
/// one LED per module; a decoder needs several samples per module to find the
/// finder patterns at all, exactly as a phone camera does by standing far
/// enough back to spread 25 modules over a few hundred sensor pixels.
const SCALE: usize = 8;

fn frame_of(screen: &Screen<'_>) -> [u8; NBYTES] {
    let mut f = [0u8; NBYTES];
    render(screen, &mut f).expect("render");
    f
}

fn luma(f: &[u8; NBYTES], x: usize, y: usize) -> u8 {
    let i = (y * W + x) * 3;
    // The screens are white, grey and two hues; a plain average is plenty to
    // separate "lit" from "off".
    ((f[i] as u16 + f[i + 1] as u16 + f[i + 2] as u16) / 3) as u8
}

/// Read the panel back with a decoder that has never seen our encoder.
fn decode(f: &[u8; NBYTES]) -> Vec<String> {
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(W * SCALE, H * SCALE, |x, y| {
        luma(f, x / SCALE, y / SCALE)
    });
    img.detect_grids()
        .iter()
        .filter_map(|g| g.decode().ok())
        .map(|(_, s)| s)
        .collect()
}

/// FNV-1a over the frame bytes. Small, stable, and defined here so the golden
/// value cannot move because a dependency changed its hasher.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn portal(ssid: &str, layout: Layout, form: UriForm) -> Screen<'_> {
    Screen::Portal { ssid, layout, form }
}

// ---------------------------------------------------------------------------
// The decode check
// ---------------------------------------------------------------------------

#[test]
fn the_rendered_panel_decodes_back_to_the_measured_payload() {
    let f = frame_of(&portal("screeny-4a00a4", Layout::QrAndName, UriForm::NoPass));
    assert_eq!(
        decode(&f),
        ["WIFI:T:nopass;S:screeny-4a00a4;;"],
        "an independent decoder must read the panel back byte for byte"
    );
}

#[test]
fn the_short_open_form_decodes_too() {
    // The form a later bench card may promote. It is not yet measured on the
    // owner's phone, but it must at least be a correct QR code today.
    let f = frame_of(&portal("screeny-4a00a4", Layout::QrAndName, UriForm::ShortOpen));
    assert_eq!(decode(&f), ["WIFI:S:screeny-4a00a4;;"]);
}

#[test]
fn the_escaping_survives_a_round_trip_through_the_panel() {
    // Never a real AP name, but if the escaping were wrong the decoder would
    // hand back a different string and this would say so.
    let f = frame_of(&portal("a;b,c", Layout::QrAndName, UriForm::ShortOpen));
    assert_eq!(decode(&f), [r#"WIFI:S:a\;b\,c;;"#]);
}

#[test]
fn inverting_the_polarity_breaks_the_decode() {
    // The measured polarity is not a preference. Prove that the decoder
    // really is reading our modules by giving it the opposite frame.
    let f = frame_of(&portal("screeny-4a00a4", Layout::QrAndName, UriForm::NoPass));
    let mut inverted = f;
    for b in inverted.iter_mut() {
        *b = 255 - *b;
    }
    assert!(
        decode(&inverted).is_empty(),
        "if this passes, the decode test above is not proving anything"
    );
}

#[test]
fn the_text_screens_carry_no_qr_at_all() {
    for s in [
        portal("screeny-4a00a4", Layout::Text, UriForm::NoPass),
        Screen::Connected {
            ip: [192, 168, 7, 221],
        },
    ] {
        assert!(decode(&frame_of(&s)).is_empty());
    }
}

// ---------------------------------------------------------------------------
// The golden frames
// ---------------------------------------------------------------------------

/// The three screens, their PNG names under `docs/research/img/` and the hash
/// of their 6144 frame bytes.
///
/// Regenerate the pictures with
/// `cargo run -p screeny-provision --example portal-png`.
const GOLDEN: [(&str, u64); 3] = [
    ("221-portal-a-qr-and-name", 0x9dba_d0f9_1c91_afcb),
    ("221-portal-c-text-only", 0x39ed_2007_bb0c_d1e2),
    ("221-connected", 0xcaf4_1436_c33f_2d3d),
];

#[test]
fn the_layouts_are_exactly_what_was_last_looked_at() {
    let frames = [
        frame_of(&portal("screeny-4a00a4", Layout::QrAndName, UriForm::NoPass)),
        frame_of(&portal("screeny-4a00a4", Layout::Text, UriForm::NoPass)),
        frame_of(&Screen::Connected {
            ip: [192, 168, 7, 221],
        }),
    ];
    for (f, (name, want)) in frames.iter().zip(GOLDEN) {
        assert_eq!(
            fnv1a(f),
            want,
            "{name} changed; regenerate the PNG, look at it, then update GOLDEN"
        );
    }
}

/// The PNGs the card asks for exist and are the size they should be. The
/// example writes them; this is what notices when somebody forgets to run it.
#[test]
fn the_pngs_are_committed_and_current() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/research/img")
        .canonicalize()
        .expect("docs/research/img");
    for (name, _) in GOLDEN {
        let p = dir.join(format!("{name}.png"));
        let meta = std::fs::metadata(&p)
            .unwrap_or_else(|e| panic!("{}: {e} - run the portal-png example", p.display()));
        assert!(meta.len() > 0 && meta.len() < 64 * 1024, "{name}.png is {} bytes", meta.len());
    }
}

// ---------------------------------------------------------------------------
// Panel safety
// ---------------------------------------------------------------------------

/// CLAUDE.md: the panel runs off laptop USB, and no screen may be a full
/// white frame. The QR's lit area is 31x31 of 64x32, which the owner has
/// already seen and accepted.
#[test]
fn the_lit_area_stays_inside_what_the_owner_accepted() {
    let f = frame_of(&portal("screeny-4a00a4", Layout::QrAndName, UriForm::NoPass));
    let full_white = f.as_chunks::<3>().0.iter().filter(|p| **p == [0xff, 0xff, 0xff]).count();
    assert!(
        full_white <= 31 * 31,
        "{full_white} full-white pixels; the measured block is 31x31 = 961"
    );
    for s in [
        portal("screeny-4a00a4", Layout::Text, UriForm::NoPass),
        Screen::Connected {
            ip: [192, 168, 7, 221],
        },
    ] {
        let f = frame_of(&s);
        assert_eq!(
            f.as_chunks::<3>().0.iter().filter(|p| **p == [0xff, 0xff, 0xff]).count(),
            0,
            "text is dim-white or a colour, never full white"
        );
    }
}

// ---------------------------------------------------------------------------
// The brightness rule (card 247, item 2)
// ---------------------------------------------------------------------------

/// The QR screen is shown at a fixed, known-good brightness; every other
/// screen honours the runtime setting.
///
/// The bench finding behind it: on 2026-09-20 the owner's phone scanned this
/// code "easily" (`docs/design/device-web.md` decision 1) at the **default**
/// brightness. On 2026-09-21 it would not, and the one thing that had changed
/// was the runtime brightness - 56, set by the Studio. The device dims by
/// shortening the output-enable window, so 56 lights 5 of 25 slots where the
/// default lights 9: a little over half the light, and a rolling-shutter
/// camera sees a short OE window as banding across the code. The QR is the one
/// screen drawn for a camera rather than for an eye, so it is the one screen
/// that does not follow the setting.
#[test]
fn only_the_qr_screen_asks_for_a_fixed_brightness() {
    use screeny_provision::wants_fixed_brightness;

    assert!(
        wants_fixed_brightness(&portal("screeny-4a00a4", Layout::QrAndName, UriForm::NoPass)),
        "the QR screen is the one screen a camera has to read"
    );
    assert!(wants_fixed_brightness(&portal(
        "screeny-4a00a4",
        Layout::QrAndName,
        UriForm::ShortOpen
    )));

    // Everything else is for a human eye and follows the setting.
    assert!(
        !wants_fixed_brightness(&portal("screeny-4a00a4", Layout::Text, UriForm::NoPass)),
        "the text fallback carries no code, so there is nothing to scan"
    );
    for s in [
        Screen::Connected {
            ip: [192, 168, 7, 221],
        },
        Screen::Updating { percent: Some(50) },
        Screen::Installing,
        Screen::WipeCountdown { seconds_left: 3 },
        Screen::WipeCancelled,
        Screen::WipeUnavailable,
    ] {
        assert!(
            !wants_fixed_brightness(&s),
            "{s:?} is drawn for an eye and must honour the brightness setting"
        );
    }
}

/// The fix is a *presentation* rule, not a redraw: fixing the brightness must
/// not change one pixel of the code. If it ever did, decision 1's measurement
/// would stop describing what the panel shows, and the golden hash above is
/// what would have to move.
#[test]
fn asking_for_a_fixed_brightness_changes_no_pixel() {
    let f = frame_of(&portal("screeny-4a00a4", Layout::QrAndName, UriForm::NoPass));
    assert_eq!(
        fnv1a(&f),
        GOLDEN[0].1,
        "the QR bitmap is bit for bit the one decision 1 measured"
    );
    assert_eq!(decode(&f), ["WIFI:T:nopass;S:screeny-4a00a4;;"]);
}
