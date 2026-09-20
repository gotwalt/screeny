//! The settings themselves: the types, their limits, and their defaults.
//!
//! Every limit here comes from [`screeny_proto::control`] rather than from a
//! number typed twice, so the store can never hold something `SET_WIFI` or
//! `SET_NAME` could not have produced.

use core::fmt;

use heapless::{String, Vec};
use screeny_proto::control::{IdleMode, MAX_NAME_LEN, MAX_PSK_LEN, MAX_SSID_LEN};

/// The brightness a device with nothing stored comes up at.
///
/// This is `firmware/src/display.rs`'s `DEFAULT_BRIGHTNESS`. The firmware card
/// (212) should make that constant an alias of this one; until then the two
/// are checked against each other only by eye, and by the note in
/// `crates/settings/README.md`.
pub const DEFAULT_BRIGHTNESS: u8 = 96;

/// Why a value was refused. Every one of these is caught before anything is
/// written to flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingError {
    /// An SSID of zero bytes. Spec section 8.2 says `ssid_len` is `1..=32`,
    /// and this crate uses the empty SSID as the "no credentials" marker in
    /// flash, so it is not a value a caller may supply.
    SsidEmpty,
    /// An SSID longer than [`MAX_SSID_LEN`].
    SsidTooLong,
    /// A PSK longer than [`MAX_PSK_LEN`].
    PskTooLong,
    /// A name longer than [`MAX_NAME_LEN`] bytes.
    NameTooLong,
    /// A name that is not UTF-8.
    NameNotUtf8,
}

/// A Wi-Fi SSID: 1 to [`MAX_SSID_LEN`] bytes.
///
/// **Bytes, not text.** 802.11 SSIDs are an opaque byte string; plenty of real
/// access points are not UTF-8. Nothing here requires UTF-8, and
/// [`Ssid::as_str`] is the only place that asks.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ssid(Vec<u8, MAX_SSID_LEN>);

impl Ssid {
    /// Validate and copy an SSID.
    ///
    /// # Errors
    /// [`SettingError::SsidEmpty`] or [`SettingError::SsidTooLong`].
    pub fn new(bytes: &[u8]) -> Result<Self, SettingError> {
        if bytes.is_empty() {
            return Err(SettingError::SsidEmpty);
        }
        Vec::from_slice(bytes)
            .map(Self)
            .map_err(|_| SettingError::SsidTooLong)
    }

    /// The bytes on the wire and in flash.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The SSID as text, when it happens to be UTF-8.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.0).ok()
    }

    /// Length in bytes. Never zero.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always false; an [`Ssid`] cannot be built empty. Present so clippy and
    /// the reader both stop asking.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }
}

/// A Wi-Fi PSK: 0 to [`MAX_PSK_LEN`] bytes, where 0 means an open network.
///
/// Spec section 8.4: *the PSK is never formatted*. That invariant is this
/// type's whole job, so its [`fmt::Debug`] prints the length and nothing else,
/// and there is deliberately no `Display`, no `as_str` and no `Deref`. The one
/// way to the bytes is [`Psk::as_bytes`], which a reviewer can grep for.
#[derive(Clone, PartialEq, Eq)]
pub struct Psk(Vec<u8, MAX_PSK_LEN>);

impl Psk {
    /// Validate and copy a PSK. An empty slice is an open network.
    ///
    /// # Errors
    /// [`SettingError::PskTooLong`].
    pub fn new(bytes: &[u8]) -> Result<Self, SettingError> {
        Vec::from_slice(bytes)
            .map(Self)
            .map_err(|_| SettingError::PskTooLong)
    }

    /// An open network.
    #[must_use]
    pub fn open() -> Self {
        Self(Vec::new())
    }

    /// The secret. Callers hand this to the radio and to nothing else.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True for an open network.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Psk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Psk(<{} bytes>)", self.0.len())
    }
}

/// A stored network: an SSID and the PSK for it.
///
/// `Debug` is derived, which is safe because [`Psk`]'s `Debug` is not.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Wifi {
    /// The network name.
    pub ssid: Ssid,
    /// The key. Empty means an open network.
    pub psk: Psk,
}

impl Wifi {
    /// Validate and copy a credential pair.
    ///
    /// # Errors
    /// Whatever [`Ssid::new`] or [`Psk::new`] refuse.
    pub fn new(ssid: &[u8], psk: &[u8]) -> Result<Self, SettingError> {
        Ok(Self {
            ssid: Ssid::new(ssid)?,
            psk: Psk::new(psk)?,
        })
    }
}

/// The friendly name, at most [`MAX_NAME_LEN`] bytes.
///
/// **UTF-8 is required here, unlike the SSID.** The name is not somebody
/// else's identifier: it comes in over `SET_NAME`, it is kept by
/// `screeny-receiver` in a `heapless::String`, and it becomes the mDNS
/// instance name, so a non-UTF-8 name is not representable further down the
/// pipe. A stored name that is not UTF-8 is therefore treated as unusable at
/// load and reported, not passed on.
///
/// Empty is the default and means "use `screeny-<id>`".
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Name(String<MAX_NAME_LEN>);

impl Name {
    /// The empty name: "use `screeny-<id>`".
    #[must_use]
    pub fn empty() -> Self {
        Self(String::new())
    }

    /// Validate and copy a name from text.
    ///
    /// # Errors
    /// [`SettingError::NameTooLong`].
    pub fn new(s: &str) -> Result<Self, SettingError> {
        let mut out = String::new();
        out.push_str(s).map_err(|_| SettingError::NameTooLong)?;
        Ok(Self(out))
    }

    /// Validate and copy a name from bytes, as they come out of flash.
    ///
    /// # Errors
    /// [`SettingError::NameNotUtf8`] or [`SettingError::NameTooLong`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SettingError> {
        let s = core::str::from_utf8(bytes).map_err(|_| SettingError::NameNotUtf8)?;
        Self::new(s)
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The bytes that go to flash.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// True when no name is set and `screeny-<id>` should be used.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Everything the device persists.
///
/// `Clone + PartialEq` so the firmware can keep one of these as the live
/// state, compare it with what came out of flash, and hand a copy to whoever
/// is rendering the settings page.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Settings {
    /// The stored network, or `None` when there is none and the device should
    /// bring up the setup portal.
    pub wifi: Option<Wifi>,
    /// The friendly name; empty means `screeny-<id>`.
    pub name: Name,
    /// Panel brightness, 0-255, clamped by the firmware cap when applied.
    pub brightness: u8,
    /// What to do when no source is streaming.
    pub idle_mode: IdleMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            wifi: None,
            name: Name::empty(),
            brightness: DEFAULT_BRIGHTNESS,
            idle_mode: IdleMode::Status,
        }
    }
}
