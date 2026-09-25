//! The `WIFI:` URI a phone's camera turns into "join this network", and the
//! length rule that keeps it inside a version 2-L QR code.
//!
//! The convention is ZXing's "barcode contents", which is what Android's own
//! `WifiQrCode.java` implements and what iOS Camera has parsed since iOS 11:
//! <https://github.com/zxing/zxing/wiki/Barcode-Contents>. The normative
//! Wi-Fi Alliance WPA3 specification v3.5 section 7.1 percent-encodes special
//! characters instead, and Android does not understand that, so an SSID
//! outside `A-Za-z0-9-` has no portable encoding at all. Ours never is one:
//! the AP name is always `screeny-<id>` (research 007 section 9.1).
//!
//! # The length rule
//!
//! The author measured on 2026-09-19 that `WIFI:T:nopass;S:screeny-c0ffee;;`
//! is **exactly** the 32 bytes a version 2-L code holds in byte mode, and
//! that the resulting 25x25 block scans easily off the panel. One more byte
//! is version 3 (29x29), which with a 1-pixel quiet zone is 31 of the panel's
//! 32 rows and leaves no room for text at all. So this module **refuses** an
//! SSID that does not fit rather than quietly growing the code:
//! [`SSID_MAX_NOPASS`] is 14 characters, and `screeny-c0ffee` is exactly 14.

use heapless::String;

/// Byte capacity of a version 2, error-correction-L QR code in byte mode.
///
/// From the QR Model 2 tables: version 2-L holds 34 data codewords, of which
/// a byte-mode segment spends 1 nibble on the mode indicator and 1 byte on
/// the character count, leaving 32.
pub const QR_V2L_BYTES: usize = 32;

/// Longest URI this module will build. Two bytes of slack over
/// [`QR_V2L_BYTES`] so that a too-long SSID is reported as
/// [`UriError::TooLong`] instead of overflowing the buffer first.
pub const URI_MAX: usize = QR_V2L_BYTES + 2;

/// Which of the two `WIFI:` spellings to build.
///
/// Research 007 section 9.1: both are understood by ZXing's parser, the short
/// one is nine bytes shorter, and the short one has **not** been scanned off
/// this panel by the author's phone yet. Ship [`UriForm::NoPass`]; a later
/// bench card flips the default if the short form reads on both an iPhone and
/// an Android phone, and that is this enum's whole reason for existing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UriForm {
    /// `WIFI:T:nopass;S:<ssid>;;` - 18 bytes of frame, measured and scanned.
    #[default]
    NoPass,
    /// `WIFI:S:<ssid>;;` - 9 bytes of frame. The Wi-Fi Alliance spec omits
    /// `T` entirely for an unauthenticated network and ZXing defaults an
    /// absent `T` to `nopass`, so one string satisfies both readings.
    /// **Not yet measured on the panel.**
    ShortOpen,
}

impl UriForm {
    /// The fixed bytes this form spends around the SSID.
    #[must_use]
    pub const fn frame_len(self) -> usize {
        match self {
            // "WIFI:T:nopass;S:" + ";;"
            UriForm::NoPass => 18,
            // "WIFI:S:" + ";;"
            UriForm::ShortOpen => 9,
        }
    }

    /// Longest *unescaped* SSID this form can carry in a version 2-L code.
    ///
    /// An SSID containing one of the five ZXing metacharacters costs an extra
    /// byte each, so this is an upper bound; [`fits`] is the exact answer.
    #[must_use]
    pub const fn ssid_max(self) -> usize {
        QR_V2L_BYTES - self.frame_len()
    }
}

/// Longest SSID the shipped form can carry: 14 characters, which is exactly
/// `screeny-<6 hex>`.
pub const SSID_MAX_NOPASS: usize = UriForm::NoPass.ssid_max();

/// Longest SSID the short open form could carry, if a bench card ever
/// promotes it: 23 characters.
pub const SSID_MAX_SHORT: usize = UriForm::ShortOpen.ssid_max();

const _: () = assert!(SSID_MAX_NOPASS == 14);
const _: () = assert!(SSID_MAX_SHORT == 23);

/// Why a `WIFI:` URI could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UriError {
    /// The SSID was empty. 802.11 allows it; a QR code for it is useless.
    Empty,
    /// The escaped payload would not fit a version 2-L code, so the encoder
    /// would have had to move to version 3 and off the panel's layout. The
    /// caller shows the text-only screen instead.
    TooLong,
}

/// The five characters ZXing backslash-escapes inside a `WIFI:` field.
const META: [char; 5] = ['\\', ';', ',', ':', '"'];

/// Build the `WIFI:` URI for an **open** network named `ssid`.
///
/// # Errors
///
/// [`UriError::Empty`] for an empty SSID, [`UriError::TooLong`] when the
/// escaped result would need a QR version above 2.
pub fn wifi_uri(form: UriForm, ssid: &str) -> Result<String<URI_MAX>, UriError> {
    if ssid.is_empty() {
        return Err(UriError::Empty);
    }
    let mut s: String<URI_MAX> = String::new();
    let head = match form {
        UriForm::NoPass => "WIFI:T:nopass;S:",
        UriForm::ShortOpen => "WIFI:S:",
    };
    // Every `push` below is checked: an SSID long enough to overflow `URI_MAX`
    // is exactly the SSID that does not fit a version 2-L code anyway, so the
    // two failures are the same failure and both report `TooLong`.
    s.push_str(head).map_err(|_| UriError::TooLong)?;
    for c in ssid.chars() {
        if META.contains(&c) {
            s.push('\\').map_err(|_| UriError::TooLong)?;
        }
        s.push(c).map_err(|_| UriError::TooLong)?;
    }
    s.push_str(";;").map_err(|_| UriError::TooLong)?;
    if s.len() > QR_V2L_BYTES {
        return Err(UriError::TooLong);
    }
    Ok(s)
}

/// Whether `ssid` fits a version 2-L code in this form, escaping included.
///
/// This is the question the portal screen asks before choosing a layout: a
/// name that does not fit gets the text-only screen rather than a QR code
/// that would not fit the panel.
#[must_use]
pub fn fits(form: UriForm, ssid: &str) -> bool {
    wifi_uri(form, ssid).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_measured_payload_is_exactly_32_bytes() {
        let u = wifi_uri(UriForm::NoPass, "screeny-4a00a4").unwrap();
        assert_eq!(u.as_str(), "WIFI:T:nopass;S:screeny-4a00a4;;");
        assert_eq!(u.len(), 32, "research 007 section 9.1 measured 32 bytes");
    }

    #[test]
    fn fourteen_fits_and_fifteen_is_refused() {
        let ok: String<16> = core::iter::repeat_n('a', 14).collect();
        let too: String<16> = core::iter::repeat_n('a', 15).collect();
        assert!(fits(UriForm::NoPass, &ok));
        assert_eq!(
            wifi_uri(UriForm::NoPass, &too),
            Err(UriError::TooLong),
            "a 15-byte SSID needs version 3; refuse rather than grow"
        );
    }

    #[test]
    fn short_form_is_nine_bytes_shorter_and_reaches_23() {
        let short = wifi_uri(UriForm::ShortOpen, "screeny-4a00a4").unwrap();
        assert_eq!(short.as_str(), "WIFI:S:screeny-4a00a4;;");
        assert_eq!(short.len(), 23);
        let n23: String<32> = core::iter::repeat_n('a', 23).collect();
        let n24: String<32> = core::iter::repeat_n('a', 24).collect();
        assert!(fits(UriForm::ShortOpen, &n23));
        assert!(!fits(UriForm::ShortOpen, &n24));
    }

    #[test]
    fn metacharacters_are_backslash_escaped() {
        // Never a real screeny AP name, but the rule is implemented and
        // therefore tested: research 007 section 9.1.
        let u = wifi_uri(UriForm::ShortOpen, r#"a\b;c,d:e"f"#).unwrap();
        assert_eq!(u.as_str(), r#"WIFI:S:a\\b\;c\,d\:e\"f;;"#);
    }

    #[test]
    fn an_escape_costs_a_byte_of_the_budget() {
        // 14 plain characters fit; 14 characters one of which is a semicolon
        // escapes to 15 and does not.
        let plain: String<16> = core::iter::repeat_n('a', 14).collect();
        assert!(fits(UriForm::NoPass, &plain));
        let mut meta: String<16> = core::iter::repeat_n('a', 13).collect();
        meta.push(';').unwrap();
        assert!(!fits(UriForm::NoPass, &meta));
    }

    #[test]
    fn an_empty_ssid_is_refused() {
        assert_eq!(wifi_uri(UriForm::NoPass, ""), Err(UriError::Empty));
        assert!(!fits(UriForm::NoPass, ""));
    }

    #[test]
    fn non_ascii_is_counted_in_bytes_not_characters() {
        // `fits` must not be fooled by multi-byte UTF-8: the QR budget is in
        // bytes. Five 3-byte characters are 15 bytes and must not fit.
        assert!(!fits(UriForm::NoPass, "\u{4e00}\u{4e00}\u{4e00}\u{4e00}\u{4e00}"));
        assert!(fits(UriForm::NoPass, "\u{4e00}\u{4e00}\u{4e00}\u{4e00}"));
    }
}
