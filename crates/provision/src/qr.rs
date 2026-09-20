//! The QR encoder wrapper: `qrcodegen-no-heap` pinned to exactly the code the
//! owner scanned off the panel, and nothing else.
//!
//! Version **2**, error correction **L**, **byte** mode, 25x25 modules. Not a
//! range, not "whatever fits": the panel layout is built around a 25x25 block
//! with a 3-pixel quiet zone (31x31 of a 64x32 panel), and a version 3 code
//! does not fit beside text at all. Anything that would need version 3 is
//! refused here and the caller shows the text-only screen instead.
//!
//! The encoder's buffers are two 80-byte arrays on this function's stack;
//! the [`Qr`] it returns owns a 79-byte bitmap and borrows nothing, so no
//! caller has to size a buffer it does not understand.

use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

/// The only QR version this crate emits.
pub const QR_VERSION: u8 = 2;

/// Modules across a version 2 code.
pub const QR_MODULES: usize = 25;

/// `Version::new(2).buffer_len()` = (25*25 + 7)/8 + 1 = 80. The encoder needs
/// two buffers of at least this size.
const QR_BUF: usize = Version::new(QR_VERSION).buffer_len();

/// Bytes of the owned module bitmap: 625 bits.
const MODULE_BYTES: usize = (QR_MODULES * QR_MODULES).div_ceil(8);

const _: () = assert!(QR_BUF == 80);
const _: () = assert!(MODULE_BYTES == 79);
const _: () = assert!(QR_MODULES == 4 * QR_VERSION as usize + 17);

/// Why a payload could not be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum QrError {
    /// The payload needs a QR version above 2, which the panel has no layout
    /// for. [`crate::uri::wifi_uri`] refuses these first; this is the
    /// backstop for a caller that built a payload some other way.
    TooLong,
}

/// A rendered version 2-L QR code: 25x25 modules, owned, `Copy`.
///
/// 80 bytes on the stack or in a struct. Deliberately not borrowing the
/// encoder's scratch buffers, which is what `qrcodegen-no-heap`'s own
/// `QrCode<'a>` does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Qr {
    modules: [u8; MODULE_BYTES],
}

impl Qr {
    /// Modules across. Always [`QR_MODULES`].
    #[must_use]
    pub const fn size(&self) -> usize {
        QR_MODULES
    }

    /// Is the module at `(x, y)` dark?
    ///
    /// Out of range is light, so a caller drawing the quiet zone can sweep a
    /// rectangle larger than the code without a bounds check of its own.
    #[must_use]
    pub fn dark(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x as usize >= QR_MODULES || y as usize >= QR_MODULES {
            return false;
        }
        let i = y as usize * QR_MODULES + x as usize;
        self.modules[i / 8] & (1 << (i % 8)) != 0
    }
}

impl core::fmt::Debug for Qr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The payload is a network name, not a secret, but dumping 625 bits
        // into a log line helps nobody.
        write!(f, "Qr({0}x{0})", QR_MODULES)
    }
}

/// Encode `payload` as a version 2-L byte-mode QR code.
///
/// Byte mode is forced (`encode_binary`) rather than left to
/// `encode_text`'s numeric/alphanumeric/byte choice, so that the code on the
/// panel does not silently change shape when a future AP name happens to be
/// all upper case. For the measured payload the two agree module for module -
/// `tests/render.rs` proves it.
///
/// # Errors
///
/// [`QrError::TooLong`] if the payload does not fit version 2 at ECC L, i.e.
/// is longer than 32 bytes.
pub fn encode(payload: &str) -> Result<Qr, QrError> {
    let mut data = [0u8; QR_BUF];
    let mut out = [0u8; QR_BUF];
    let len = payload.len();
    if len > crate::uri::QR_V2L_BYTES {
        return Err(QrError::TooLong);
    }
    data[..len].copy_from_slice(payload.as_bytes());

    let code = QrCode::encode_binary(
        &mut data,
        len,
        &mut out,
        QrCodeEcc::Low,
        Version::new(QR_VERSION),
        Version::new(QR_VERSION),
        None,
        // `boostecl: false`. A boosted ECC level cannot shrink the code and
        // would change the module pattern away from the one the owner
        // scanned; we want the measured code, not the strongest one.
        false,
    )
    .map_err(|_| QrError::TooLong)?;

    debug_assert_eq!(code.size() as usize, QR_MODULES);
    let mut modules = [0u8; MODULE_BYTES];
    for y in 0..QR_MODULES {
        for x in 0..QR_MODULES {
            if code.get_module(x as i32, y as i32) {
                let i = y * QR_MODULES + x;
                modules[i / 8] |= 1 << (i % 8);
            }
        }
    }
    Ok(Qr { modules })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uri::{wifi_uri, UriForm};

    #[test]
    fn the_measured_payload_encodes_to_25x25() {
        let uri = wifi_uri(UriForm::NoPass, "screeny-4a00a4").unwrap();
        let qr = encode(&uri).unwrap();
        assert_eq!(qr.size(), 25);
        // Finder patterns: a 7x7 ring in three corners.
        for (ox, oy) in [(0, 0), (18, 0), (0, 18)] {
            assert!(qr.dark(ox, oy));
            assert!(qr.dark(ox + 6, oy + 6));
            assert!(!qr.dark(ox + 1, oy + 1));
            assert!(qr.dark(ox + 3, oy + 3));
        }
    }

    #[test]
    fn out_of_range_modules_are_light() {
        let qr = encode("WIFI:S:x;;").unwrap();
        assert!(!qr.dark(-1, 0));
        assert!(!qr.dark(0, -1));
        assert!(!qr.dark(25, 0));
        assert!(!qr.dark(0, 25));
    }

    #[test]
    fn a_33_byte_payload_is_refused() {
        let long: heapless::String<40> = core::iter::repeat('a').take(33).collect();
        assert_eq!(encode(&long), Err(QrError::TooLong));
    }

    #[test]
    fn debug_does_not_dump_the_bitmap() {
        let qr = encode("WIFI:S:x;;").unwrap();
        let mut s: heapless::String<32> = heapless::String::new();
        core::fmt::write(&mut s, format_args!("{qr:?}")).unwrap();
        assert_eq!(s.as_str(), "Qr(25x25)");
    }
}
