//! Card 201 spike, part 6: the portal screen and its WiFi QR.

use crate::display::Frame;
use crate::{COLS, ROWS};

// ---------------------------------------------------------------------------
// 6: the portal screen
// ---------------------------------------------------------------------------

/// Longest SSID that still fits a version 2-L QR in byte mode (32 bytes).
/// The frame `WIFI:T:nopass;S:;;` is 18 bytes, so the SSID may be 14 -
/// exactly the length of `screeny-4a00a4`.
pub const QR_V2_SSID_MAX: usize = 32 - 18;

/// `Version(3).buffer_len()` = (29*29 + 7)/8 + 1 = 107 bytes, needed twice.
const QR_MAX_VERSION: u8 = 3;
const QR_BUF: usize = 107;

/// Draw the portal screen: the WiFi QR on the left, the SSID on the right.
///
/// Standard polarity - the quiet zone and the light modules lit white, the
/// dark modules off - because that is what the owner scanned "easily" on the
/// bench on 2026-09-19.
pub fn portal_screen(frame: &mut Frame, ssid: &str) -> bool {
    use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

    frame.clear();

    let mut payload = heapless::String::<64>::new();
    if payload.push_str("WIFI:T:nopass;S:").is_err()
        || payload.push_str(ssid).is_err()
        || payload.push_str(";;").is_err()
    {
        return false;
    }

    let mut tmp = [0u8; QR_BUF];
    let mut out = [0u8; QR_BUF];
    let Ok(qr) = QrCode::encode_text(
        payload.as_str(),
        &mut tmp,
        &mut out,
        QrCodeEcc::Low,
        Version::new(1),
        Version::new(QR_MAX_VERSION),
        None,
        // `boostecl: false` — a boosted ECC level changes nothing about size
        // but costs encode time, and we want the smallest version, not the
        // strongest code.
        false,
    ) else {
        return false;
    };

    let size = qr.size(); // 25 at version 2, 29 at version 3
    if size as usize > ROWS {
        return false;
    }

    // One LED per module. The quiet zone is whatever is left of the 32 rows,
    // capped at 3 (the bench value); the block is left-aligned.
    let quiet = ((ROWS as i32 - size) / 2).min(3);
    let x0 = quiet;
    let y0 = (ROWS as i32 - size) / 2;
    let white = [0xff, 0xff, 0xff];
    for y in -quiet..size + quiet {
        for x in -quiet..size + quiet {
            // A *set* module is dark; everything else, quiet zone included,
            // is lit.
            if !qr.get_module(x, y) {
                let (px, py) = (x0 + x, y0 + y);
                if px >= 0 && py >= 0 {
                    frame.set(px as usize, py as usize, white);
                }
            }
        }
    }

    // Whatever is left of the 64 columns carries the name and the instruction;
    // `screens.rs` owns the text drawing, so the spike only reserves the box.
    let text_x = (x0 + size + quiet + 1) as usize;
    debug_assert!(text_x < COLS);
    true
}
