//! The device's HTTP API, defined once.
//!
//! The firmware serves it (card 222), the simulator serves the same thing on a
//! host port (card 224), and the Studio reads it. Three programs, one set of
//! shapes: if the firmware grows a field, the Studio's parser gets it in the
//! same commit or the build breaks. That is the whole job of this crate - it
//! serves nothing and talks to nothing.
//!
//! The route table is research 007 section 7, with card 226's adjustments.
//! `crates/device-api/README.md` has it in one page; the golden files under
//! `tests/golden/` are the examples, and they are checked against these types
//! in both directions on every `cargo test`.
//!
//! # Rules this crate keeps
//!
//! * **`no_std`, no alloc, no float.** Every string is a
//!   `heapless::String<N>`, every list a `heapless::Vec<T, N>`, and every `N`
//!   comes from `screeny-proto` or from a named constant here. The firmware
//!   links it; nothing it pulls in is anything the firmware would not already
//!   have.
//! * **No PSK, anywhere, ever.** No reply type has a field for one (spec
//!   section 8.4). `tests/no_psk.rs` serialises a worst-case value of every
//!   reply and greps the bytes for `psk` and `pass`, so the invariant is
//!   checked rather than remembered. The one type that holds a PSK is
//!   [`form::WifiForm`], which is a *request*, whose `Debug` prints the
//!   length and never the bytes.
//! * **Both serialisers, both directions.** The firmware writes JSON with
//!   picoserve's writer and reads it with `serde-json-core`; the Studio uses
//!   `serde_json`. The tests round-trip every type through `serde-json-core`
//!   and `serde_json` and compare against the same golden bytes.
//!
//! # `MAX_JSON_LEN`: what it is for
//!
//! Every request and reply type has one, and every one is proved by a test
//! that serialises a worst-case value. What they are good for differs:
//!
//! * On a **request** type it is the size of the buffer the body has to land
//!   in. picoserve's `Json` and `Form` extractors call `read_all()`, which
//!   needs the whole body contiguous in the HTTP buffer, so
//!   [`route::MAX_REQUEST_LEN`] is a real dimension of card 222's server.
//! * On a **reply** type it is *not* picoserve's buffer size. picoserve
//!   measures a JSON reply by serialising it into a counting writer
//!   (`Content::content_length`) and then streams it, so no reply needs a
//!   buffer at all. The reply bound is for everyone else: `serde-json-core`'s
//!   `to_slice` into a fixed array, a test, and the honest answer to "how big
//!   can this get".
//!
//! The bounds assume the worst case for text: every byte of a name or an SSID
//! escaping to six characters (`\u001f`). Real ones are a twentieth of that.
//!
//! # One thing to know about the three JSON writers
//!
//! picoserve escapes `/` as `\/`; `serde_json` and `serde-json-core` do not.
//! `serde-json-core` writes `\u00XX` in upper-case hex; the other two use
//! lower case. Every one of the three parses all of it, so no consumer is
//! affected - but two byte-for-byte comparisons of the same value can differ.
//! No golden file in this crate contains a `/` or a control character inside a
//! string, and `tests/golden.rs` enforces that, so every golden is what all
//! three writers produce.
//!
//! # Example
//!
//! ```
//! use screeny_device_api::{form, reply::AcceptedReply, route};
//!
//! // The portal's form post, from an iOS captive mini-browser.
//! let form = form::parse_wifi_form(b"ssid=Example-Wifi1&psk=password9").unwrap();
//! assert_eq!(form.ssid(), b"Example-Wifi1");
//! assert!(!form.is_open());
//! // The PSK is not in the Debug output, ever.
//! assert!(!format!("{form:?}").contains("password9"));
//!
//! // ...and the reply goes out before the radio work starts (spec 8.2).
//! assert_eq!(route::WIFI, "/api/v1/wifi");
//! assert_eq!(
//!     serde_json::to_string(&AcceptedReply::TRYING).unwrap(),
//!     r#"{"result":"trying"}"#
//! );
//! ```

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

#[cfg(any(test, feature = "std"))]
extern crate std;

pub mod enums;
pub mod error;
pub mod form;
pub mod reply;
pub mod request;
pub mod route;
pub mod text;

pub use enums::{
    Accepted, FailReason, FirmwareError, FwSlot, FwState, IdleMode, ResetReason, StreamState,
    WifiState,
};
pub use error::{ErrorCode, ErrorReply};
pub use route::{Method, RateLimit, Route, ROUTES};

/// The version in every path, and the `api` field of
/// [`StatusReply`](reply::StatusReply).
pub const API_VERSION: u8 = route::API_VERSION;

/// Worst-case JSON expansion of one byte of arbitrary text.
///
/// A control character below `0x20` that has no short escape becomes
/// `\u001f`: six characters for one byte. Nothing expands further - a
/// multi-byte UTF-8 character is never escaped by any of the three writers, so
/// its bytes pass through one for one. Every text field's contribution to a
/// `MAX_JSON_LEN` is `N * ESCAPE_MAX`.
pub(crate) const ESCAPE_MAX: usize = 6;

/// `"4294967295"`.
pub(crate) const MAX_U32_LEN: usize = 10;
/// `"65535"`.
pub(crate) const MAX_U16_LEN: usize = 5;
/// `"255"`.
pub(crate) const MAX_U8_LEN: usize = 3;
/// `"-128"`.
pub(crate) const MIN_I8_LEN: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    #[test]
    fn the_number_widths_are_right() {
        assert_eq!(u32::MAX.to_string().len(), MAX_U32_LEN);
        assert_eq!(u16::MAX.to_string().len(), MAX_U16_LEN);
        assert_eq!(u8::MAX.to_string().len(), MAX_U8_LEN);
        assert_eq!(i8::MIN.to_string().len(), MIN_I8_LEN);
        // i8::MIN is the longest i8, not i8::MAX.
        assert!(i8::MAX.to_string().len() <= MIN_I8_LEN);
    }

    #[test]
    fn the_api_version_is_the_one_in_the_paths() {
        assert_eq!(API_VERSION, 1);
        assert!(route::PREFIX.ends_with("/v1"));
    }
}
