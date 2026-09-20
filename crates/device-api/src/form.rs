//! `POST /api/v1/wifi`: an `application/x-www-form-urlencoded` body, parsed
//! without an allocator and without assuming the SSID is text.
//!
//! # Why this is not `picoserve::extract::Form`
//!
//! Two reasons, both structural.
//!
//! 1. **An 802.11 SSID is a byte string, not text.** picoserve's `Form`
//!    extractor rejects a body that is not UTF-8 outright
//!    (`FormRejection::BodyIsNotUtf8`) and deserialises into
//!    `serde::Deserialize` types, so the SSID would have to be a `String`.
//!    Plenty of real access points are named in some other encoding, and
//!    `crates/settings`'s [`Ssid`](screeny_settings_docs) is bytes for exactly
//!    that reason. This parser hands the caller the bytes.
//! 2. **The PSK must not be printable.** [`WifiForm`]'s `Debug` prints the
//!    SSID and the PSK's *length*, and there is no `Display`, no `Deref` and
//!    no `as_str`. The one way to the secret is [`WifiForm::psk`], which a
//!    reviewer can grep for. That is the same shape `crates/settings`'s `Psk`
//!    takes, for the same reason (spec section 8.4).
//!
//! # Why a form at all
//!
//! Research 007 section 4.4: the iOS captive mini-browser only re-probes the
//! network on a **full-page navigation**, so the provisioning path is a plain
//! `<form method=post>` and a full-page result, not `fetch()`. A form post is
//! urlencoded; that is the whole reason this file exists rather than a JSON
//! request type.
//!
//! [`screeny_settings_docs`]: https://docs.rs/

use core::fmt;

use heapless::Vec;

use crate::error::{ErrorCode, ErrorReply};
use crate::text::{PinText, MAX_PIN_LEN, MAX_PSK_LEN, MAX_SSID_LEN};

/// The longest body this parser will look at.
///
/// Worst case, with every byte of every value percent-encoded as `%XX`:
///
/// ```text
///   "ssid="     5 + 32*3 =  101
///   "&psk="     5 + 64*3 =  197
///   "&pin="     5 + 16*3 =   53
///   "&counter=" 9 + 10   =   19
///                          ----
///                           370
/// ```
///
/// Rounded up to 384. A body longer than this cannot be a valid WiFi form, so
/// card 222 can refuse it before reading it all and never has to buffer more.
pub const MAX_FORM_LEN: usize = 384;

/// Why a form body was refused.
///
/// Each one names the field, because the portal page shows this to somebody
/// standing at the device with their phone and "bad form" helps nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    /// Longer than [`MAX_FORM_LEN`].
    TooLong,
    /// A `%` not followed by two hex digits.
    BadEscape,
    /// No `ssid` key at all.
    MissingSsid,
    /// `ssid=` with nothing after it. Spec 8.2 says `ssid_len` is `1..=32`,
    /// and `crates/settings` uses the empty SSID as its "no credentials"
    /// marker, so it is not something a caller may send.
    SsidEmpty,
    /// The SSID is longer than 32 bytes once decoded.
    SsidTooLong,
    /// The PSK is longer than 64 bytes once decoded.
    PskTooLong,
    /// The PIN is longer than 16 bytes, or is not UTF-8.
    BadPin,
    /// `counter` is not a decimal `u32`.
    BadCounter,
    /// The same key appeared twice.
    ///
    /// Most form parsers take the last value. This one refuses, because this
    /// is the one form in the product where an ambiguity must not be resolved
    /// silently: `psk=right&psk=wrong` should not be a coin toss about what
    /// gets written to flash.
    DuplicateKey,
}

impl FormError {
    /// The error code to answer with.
    #[must_use]
    pub const fn code(self) -> ErrorCode {
        match self {
            FormError::TooLong => ErrorCode::PayloadTooLarge,
            FormError::SsidTooLong
            | FormError::PskTooLong
            | FormError::BadPin
            | FormError::SsidEmpty => ErrorCode::OutOfRange,
            FormError::BadEscape
            | FormError::MissingSsid
            | FormError::BadCounter
            | FormError::DuplicateKey => ErrorCode::BadForm,
        }
    }

    /// The short sentence for the person holding the phone.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            FormError::TooLong => "form body is too long",
            FormError::BadEscape => "malformed percent-escape",
            FormError::MissingSsid => "no ssid field",
            FormError::SsidEmpty => "ssid is empty",
            FormError::SsidTooLong => "ssid is longer than 32 bytes",
            FormError::PskTooLong => "password is longer than 64 bytes",
            FormError::BadPin => "pin is not 1-16 bytes of text",
            FormError::BadCounter => "counter is not a number",
            FormError::DuplicateKey => "a field was sent twice",
        }
    }

    /// The whole error reply, ready to serialise.
    #[must_use]
    pub fn reply(self) -> ErrorReply {
        ErrorReply::with_detail(self.code(), self.detail())
    }
}

impl fmt::Display for FormError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.detail())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for FormError {}

/// A parsed `ssid=&psk=` form.
///
/// `Debug` never prints the PSK; see the module docs.
#[derive(Clone, PartialEq, Eq)]
pub struct WifiForm {
    ssid: Vec<u8, MAX_SSID_LEN>,
    psk: Vec<u8, MAX_PSK_LEN>,
    /// Decision 3; parsed and ignored.
    pub pin: Option<PinText>,
    /// Decision 3; parsed and ignored.
    pub counter: Option<u32>,
}

impl WifiForm {
    /// The network name, as bytes. Never empty.
    #[must_use]
    pub fn ssid(&self) -> &[u8] {
        &self.ssid
    }

    /// The secret. Callers hand this to the radio and to nothing else.
    #[must_use]
    pub fn psk(&self) -> &[u8] {
        &self.psk
    }

    /// How long the PSK is. Safe to log; the PSK itself is not.
    #[must_use]
    pub fn psk_len(&self) -> usize {
        self.psk.len()
    }

    /// True when no password was given, which is how an open network is
    /// requested.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.psk.is_empty()
    }

    /// The PIN and counter, for [`crate::request::check_auth`].
    #[must_use]
    pub fn auth(&self) -> crate::request::Auth<'_> {
        crate::request::Auth {
            pin: self.pin.as_deref(),
            counter: self.counter,
        }
    }
}

impl fmt::Debug for WifiForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WifiForm")
            .field("ssid", &SsidDebug(&self.ssid))
            .field("psk_len", &self.psk.len())
            .field("counter", &self.counter)
            .finish()
    }
}

struct SsidDebug<'a>(&'a [u8]);

impl fmt::Debug for SsidDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match core::str::from_utf8(self.0) {
            Ok(s) => write!(f, "{s:?}"),
            Err(_) => write!(f, "{:?}", self.0),
        }
    }
}

/// Parse an `application/x-www-form-urlencoded` body into a [`WifiForm`].
///
/// Bytes in, bytes out: the body is not required to be UTF-8, because the
/// SSID is not.
///
/// Rules, all of them tested:
/// * `+` decodes to a space; `%XX` decodes to that byte, upper or lower case
///   hex; a `%` that is not followed by two hex digits is
///   [`FormError::BadEscape`].
/// * `psk=` with nothing after it, or no `psk` key at all, is an open network.
/// * Unknown keys are ignored, so a page can add a hidden field without a
///   firmware change.
/// * A repeated key is [`FormError::DuplicateKey`].
/// * A key with no `=` (`ssid&psk=x`) is a key with an empty value.
///
/// # Errors
/// Any [`FormError`].
pub fn parse_wifi_form(body: &[u8]) -> Result<WifiForm, FormError> {
    if body.len() > MAX_FORM_LEN {
        return Err(FormError::TooLong);
    }

    let mut ssid: Option<Vec<u8, MAX_SSID_LEN>> = None;
    let mut psk: Option<Vec<u8, MAX_PSK_LEN>> = None;
    let mut pin: Option<PinText> = None;
    let mut counter: Option<u32> = None;
    let mut seen_ssid = false;
    let mut seen_psk = false;
    let mut seen_pin = false;
    let mut seen_counter = false;

    for pair in body.split(|&b| b == b'&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.iter().position(|&b| b == b'=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, &pair[pair.len()..]),
        };
        // Keys are always ASCII here - they are ours - but they may still
        // arrive percent-encoded, so decode before comparing.
        let mut key_buf: Vec<u8, 16> = Vec::new();
        if decode_into(key, &mut key_buf)?.is_err() {
            // A key longer than any of ours: an unknown key, ignored.
            continue;
        }

        match key_buf.as_slice() {
            b"ssid" => {
                if core::mem::replace(&mut seen_ssid, true) {
                    return Err(FormError::DuplicateKey);
                }
                let mut out = Vec::new();
                decode_into(value, &mut out)?.map_err(|()| FormError::SsidTooLong)?;
                ssid = Some(out);
            }
            b"psk" => {
                if core::mem::replace(&mut seen_psk, true) {
                    return Err(FormError::DuplicateKey);
                }
                let mut out = Vec::new();
                decode_into(value, &mut out)?.map_err(|()| FormError::PskTooLong)?;
                psk = Some(out);
            }
            b"pin" => {
                if core::mem::replace(&mut seen_pin, true) {
                    return Err(FormError::DuplicateKey);
                }
                let mut out: Vec<u8, MAX_PIN_LEN> = Vec::new();
                decode_into(value, &mut out)?.map_err(|()| FormError::BadPin)?;
                if out.is_empty() {
                    pin = None;
                } else {
                    let s = core::str::from_utf8(&out).map_err(|_| FormError::BadPin)?;
                    pin = Some(crate::text::text(s).ok_or(FormError::BadPin)?);
                }
            }
            b"counter" => {
                if core::mem::replace(&mut seen_counter, true) {
                    return Err(FormError::DuplicateKey);
                }
                let mut out: Vec<u8, 10> = Vec::new();
                decode_into(value, &mut out)?.map_err(|()| FormError::BadCounter)?;
                if !out.is_empty() {
                    let s = core::str::from_utf8(&out).map_err(|_| FormError::BadCounter)?;
                    counter = Some(s.parse().map_err(|_| FormError::BadCounter)?);
                }
            }
            // Unknown key: ignored on purpose, so the page can carry a hidden
            // field (a CSRF token, a "which form was this" marker) without a
            // firmware change.
            _ => {}
        }
    }

    let ssid = ssid.ok_or(FormError::MissingSsid)?;
    if ssid.is_empty() {
        return Err(FormError::SsidEmpty);
    }
    Ok(WifiForm {
        ssid,
        // No `psk` key at all means the same as `psk=`: an open network. A
        // browser form always sends the key; `curl -d 'ssid=Foo'` does not.
        psk: psk.unwrap_or_default(),
        pin,
        counter,
    })
}

/// Percent- and plus-decode `src` into `out`.
///
/// The outer `Result` is a malformed escape, which is fatal. The inner one is
/// "it did not fit", which each caller names differently.
fn decode_into<const N: usize>(
    src: &[u8],
    out: &mut Vec<u8, N>,
) -> Result<Result<(), ()>, FormError> {
    let mut i = 0;
    while i < src.len() {
        let b = match src[i] {
            b'+' => {
                i += 1;
                b' '
            }
            b'%' => {
                let hi = src.get(i + 1).copied().and_then(hex_digit);
                let lo = src.get(i + 2).copied().and_then(hex_digit);
                let (Some(hi), Some(lo)) = (hi, lo) else {
                    return Err(FormError::BadEscape);
                };
                i += 3;
                (hi << 4) | lo
            }
            other => {
                i += 1;
                other
            }
        };
        if out.push(b).is_err() {
            return Ok(Err(()));
        }
    }
    Ok(Ok(()))
}

const fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
