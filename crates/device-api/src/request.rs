//! What a caller sends.
//!
//! Only the JSON bodies are here. `POST /api/v1/wifi` is urlencoded, not JSON
//! (research 007 section 4.4: the iOS captive mini-browser path is a plain
//! form post), and lives in [`crate::form`]. `POST /api/v1/firmware` has no
//! body type at all: it is a raw `application/octet-stream` stream.
//!
//! Every mutating request carries an optional `pin` and `counter`. They are
//! **parsed and ignored today** - decision 3 of `docs/design/device-web.md`
//! says HTTP is unauthenticated on the LAN and that the API should be shaped
//! so a PIN can be added later. [`check_auth`] is the one function that will
//! grow teeth when parked card 041 is unparked; every mutating route calls it
//! now so that nothing has to be found later.

use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::enums::IdleMode;
use crate::text::{NameText, PinText, MAX_NAME_LEN, MAX_PIN_LEN};
use crate::{ESCAPE_MAX, MAX_U32_LEN, MAX_U8_LEN};

/// The unescape buffer every JSON request body must be parsed with.
///
/// **`serde_json_core::from_slice` does not unescape strings.** With no
/// buffer it hands the visitor the raw escaped text, so a name posted with a
/// six-character `\u00e9` in it arrives holding those six characters and is
/// stored that way - silently, with no error. `from_slice_escaped` is
/// the function to use, and picoserve's `Json` extractor is
/// `JsonWithUnescapeBufferSize<T, 32>` underneath.
///
/// The buffer holds one *unescaped* string at a time and is reused between
/// fields, so it has to be as long as the longest string any request type can
/// hold: [`NameText`], 32 bytes. picoserve's default of 32 is therefore
/// exactly enough and not a byte more; card 222 should spell it
/// `JsonWithUnescapeBufferSize<T, { MIN_UNESCAPE_BUFFER }>` so that raising
/// [`screeny_proto::control::MAX_NAME_LEN`] raises it too.
pub const MIN_UNESCAPE_BUFFER: usize = MAX_NAME_LEN;

/// The four ASCII bytes `POST /api/v1/reboot` must send as `confirm`.
///
/// It is the same four bytes as [`screeny_proto::control::REBOOT_MAGIC`] in
/// little-endian order, which is not a coincidence and is a test.
pub const REBOOT_CONFIRM: &str = "RBOO";

/// The optional PIN and replay counter every mutating request may carry.
///
/// Parsed and ignored today. See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Auth<'a> {
    /// The PIN, if one was sent.
    pub pin: Option<&'a str>,
    /// A monotonic counter, for replay protection, if one was sent.
    pub counter: Option<u32>,
}

/// The one place a PIN is ever checked.
///
/// Today it accepts everything, including nothing, because decision 3 says the
/// API is open on the LAN. Card 041 replaces the body and every mutating route
/// gets the new behaviour without being edited.
///
/// # Errors
/// Never, today. [`ErrorCode::Unauthorized`] once card 041 lands.
#[allow(clippy::missing_const_for_fn, clippy::needless_pass_by_value)]
pub fn check_auth(_auth: Auth<'_>) -> Result<(), ErrorCode> {
    Ok(())
}

/// A request that changes something, and so carries [`Auth`].
pub trait Mutating {
    /// The PIN and counter this request carried, if any.
    fn auth(&self) -> Auth<'_>;

    /// Shorthand for [`check_auth`] on this request.
    ///
    /// # Errors
    /// Whatever [`check_auth`] returns.
    fn check_auth(&self) -> Result<(), ErrorCode> {
        check_auth(self.auth())
    }
}

/// `POST /api/v1/settings`: change any subset of the three live settings.
///
/// Every field is optional and an absent field means "leave it alone", so a
/// brightness slider does not have to know the device's name to move. An
/// absent field is left out of the JSON, not sent as `null`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsRequest {
    /// The new friendly name. Empty means "go back to `screeny-<id>`".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<NameText>,
    /// The new brightness, 0-255, clamped by the firmware cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    /// The new idle mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_mode: Option<IdleMode>,
    /// Decision 3; parsed and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<PinText>,
    /// Decision 3; parsed and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter: Option<u32>,
}

impl SettingsRequest {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1
        + field("name", 2 + MAX_NAME_LEN * ESCAPE_MAX)
        + field("brightness", MAX_U8_LEN)
        + field("idle_mode", IdleMode::MAX_JSON_LEN)
        + field("pin", 2 + MAX_PIN_LEN * ESCAPE_MAX)
        + field("counter", MAX_U32_LEN);

    /// True when the request asks for nothing at all. A no-op is not an
    /// error, but the caller may want to skip the flash write.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.brightness.is_none() && self.idle_mode.is_none()
    }
}

impl Mutating for SettingsRequest {
    fn auth(&self) -> Auth<'_> {
        Auth {
            pin: self.pin.as_deref(),
            counter: self.counter,
        }
    }
}

/// `POST /api/v1/reboot`: `{"confirm":"RBOO"}`.
///
/// The magic word is there for the same reason `REBOOT`'s is (spec 6.9): a
/// stray POST from a crawler, a prefetcher or a captive-portal probe must not
/// reboot the panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebootRequest {
    /// Must be [`REBOOT_CONFIRM`].
    pub confirm: heapless::String<8>,
    /// Decision 3; parsed and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<PinText>,
    /// Decision 3; parsed and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter: Option<u32>,
}

impl RebootRequest {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1
        + field("confirm", 2 + 8 * ESCAPE_MAX)
        + field("pin", 2 + MAX_PIN_LEN * ESCAPE_MAX)
        + field("counter", MAX_U32_LEN);

    /// True when `confirm` is the magic word.
    #[must_use]
    pub fn confirmed(&self) -> bool {
        self.confirm == REBOOT_CONFIRM
    }
}

impl Mutating for RebootRequest {
    fn auth(&self) -> Auth<'_> {
        Auth {
            pin: self.pin.as_deref(),
            counter: self.counter,
        }
    }
}

/// `POST /api/v1/identify`: `{"duration_ms":10000}`, mirroring `IDENTIFY`.
///
/// `IDENTIFY`'s wire field is a `u16` of milliseconds (spec 6.6), so anything
/// above [`MAX_IDENTIFY_MS`] cannot be passed on and is
/// [`ErrorCode::OutOfRange`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentifyRequest {
    /// How long to show the identify screen, milliseconds.
    pub duration_ms: u32,
    /// Decision 3; parsed and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<PinText>,
    /// Decision 3; parsed and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter: Option<u32>,
}

/// The longest identify the UDP protocol can express: `IDENTIFY`'s body is a
/// little-endian `u16` of milliseconds.
pub const MAX_IDENTIFY_MS: u32 = u16::MAX as u32;

impl IdentifyRequest {
    /// An upper bound on the serialised length.
    pub const MAX_JSON_LEN: usize = 1
        + field("duration_ms", MAX_U32_LEN)
        + field("pin", 2 + MAX_PIN_LEN * ESCAPE_MAX)
        + field("counter", MAX_U32_LEN);

    /// The duration as the `u16` `IDENTIFY` carries.
    ///
    /// # Errors
    /// [`ErrorCode::OutOfRange`] above [`MAX_IDENTIFY_MS`].
    pub fn duration_u16(&self) -> Result<u16, ErrorCode> {
        u16::try_from(self.duration_ms).map_err(|_| ErrorCode::OutOfRange)
    }
}

impl Mutating for IdentifyRequest {
    fn auth(&self) -> Auth<'_> {
        Auth {
            pin: self.pin.as_deref(),
            counter: self.counter,
        }
    }
}

/// `"key":value` plus the comma or brace in front of it; see
/// [`crate::reply`]'s copy.
const fn field(key: &str, value_max: usize) -> usize {
    1 + key.len() + 2 + 1 + value_max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reboot_word_is_the_udp_magic() {
        assert_eq!(
            REBOOT_CONFIRM.as_bytes(),
            screeny_proto::control::REBOOT_MAGIC.to_le_bytes()
        );
    }

    #[test]
    fn identify_refuses_what_the_wire_cannot_carry() {
        let mut r = IdentifyRequest {
            duration_ms: 10_000,
            pin: None,
            counter: None,
        };
        assert_eq!(r.duration_u16(), Ok(10_000));
        r.duration_ms = MAX_IDENTIFY_MS;
        assert_eq!(r.duration_u16(), Ok(u16::MAX));
        r.duration_ms = MAX_IDENTIFY_MS + 1;
        assert_eq!(r.duration_u16(), Err(ErrorCode::OutOfRange));
    }

    #[test]
    fn auth_is_accepted_and_ignored() {
        let r = SettingsRequest {
            brightness: Some(96),
            pin: crate::text::text("1234"),
            counter: Some(7),
            ..SettingsRequest::default()
        };
        assert_eq!(r.check_auth(), Ok(()));
        assert_eq!(r.auth().pin, Some("1234"));
        assert_eq!(r.auth().counter, Some(7));
        // ...and with nothing at all, which is every request today.
        assert_eq!(SettingsRequest::default().check_auth(), Ok(()));
    }

    #[test]
    fn an_empty_settings_request_is_recognised() {
        assert!(SettingsRequest::default().is_empty());
        assert!(!SettingsRequest {
            brightness: Some(1),
            ..SettingsRequest::default()
        }
        .is_empty());
        // A pin on its own still asks for nothing.
        assert!(SettingsRequest {
            pin: crate::text::text("1234"),
            ..SettingsRequest::default()
        }
        .is_empty());
    }
}
