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
///
/// An SSID is not a secret (spec 8.4 is about the PSK), so `Debug` prints it:
/// as text when it is UTF-8, as bytes otherwise.
#[derive(Clone, PartialEq, Eq)]
pub struct Ssid(Vec<u8, MAX_SSID_LEN>);

impl fmt::Debug for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            Some(s) => write!(f, "Ssid({s:?})"),
            None => write!(f, "Ssid({:?})", &self.0[..]),
        }
    }
}

impl Ssid {
    /// Validate and copy an SSID.
    ///
    /// # Errors
    /// [`SettingError::SsidEmpty`] or [`SettingError::SsidTooLong`].
    pub fn new(bytes: &[u8]) -> Result<Self, SettingError> {
        if bytes.is_empty() {
            return Err(SettingError::SsidEmpty);
        }
        Vec::from_slice(bytes).map(Self).map_err(|_| SettingError::SsidTooLong)
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
        Vec::from_slice(bytes).map(Self).map_err(|_| SettingError::PskTooLong)
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
        Ok(Self { ssid: Ssid::new(ssid)?, psk: Psk::new(psk)? })
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

/// Which of the two Gen 1 HUB75 colour-line wirings this unit has.
///
/// Some units are wired in the order Tidbyt's `hdk` publishes; the first bench
/// unit is rotated relative to it (the lines the hdk calls R/G/B drive
/// blue/red/green). One firmware image serves both, and the firmware picks
/// which GPIO is which colour at boot from this setting (card 361). The wire
/// protocol never carries it.
///
/// The flash byte is [`ColourOrder::as_u8`]; an unknown byte falls back to the
/// default at load, like an unknown idle mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum ColourOrder {
    /// The first bench unit's wiring, and the default so every unit already in
    /// the field boots exactly as it did before this setting existed.
    #[default]
    Rotated = 0,
    /// Tidbyt's published (`hdk`) pin map.
    Published = 1,
}

impl ColourOrder {
    /// The flash byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Interpret a flash byte. `None` for a value this build does not know.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(ColourOrder::Rotated),
            1 => Some(ColourOrder::Published),
            _ => None,
        }
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
    /// The panel's colour-line wiring. Read once at boot to pick the pins, so
    /// a change takes effect on the next reboot.
    pub colour_order: ColourOrder,
    /// How the on-device clock looks and moves.
    pub clock: ClockConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            wifi: None,
            name: Name::empty(),
            brightness: DEFAULT_BRIGHTNESS,
            idle_mode: IdleMode::Status,
            colour_order: ColourOrder::Rotated,
            clock: ClockConfig::default(),
        }
    }
}

/// How the on-device clock looks and moves: the Studio patch's parameters, as
/// integers so they compare and store exactly. Every field is clamped on the
/// way in by [`ClockConfig::sanitised`], so a bad write cannot wedge the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClockConfig {
    /// Hour hand hue, degrees.
    pub hue: u16,
    /// Hour hand chroma, thousandths.
    pub chroma: u16,
    /// Minute hand hue, degrees.
    pub hue2: u16,
    /// Minute hand chroma, thousandths.
    pub chroma2: u16,
    /// Hand lightness, percent.
    pub light: u16,
    /// Hand weight, hundredths of an LED.
    pub weight: u16,
    /// Hand tip while dancing, percent of the weight.
    pub tip: u16,
    /// Dial rings, percent.
    pub dials: u16,
    /// Hand speed, degrees per second.
    pub speed: u16,
    /// Seconds the time is held before the hands may drift.
    pub still: u16,
    /// `0` varies, then each dance by number, then always composed.
    pub dance: u8,
    /// Which treatment the resting dials get.
    pub rest: u8,
    /// 24-hour digits.
    pub hours24: bool,
    /// Seconds per displayed minute; 60 is a real clock.
    pub pace: u16,
    /// Minutes added to the time.
    pub offset: u16,
    /// A resting dial's dark ramp.
    pub dark: u8,
    /// Tenths of a second the resting dials take to dim.
    pub fade: u16,
    /// Reduce motion.
    pub calm: bool,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl ClockConfig {
    /// The owner's "Gold and Blue" (2026-10-28).
    pub const DEFAULT: Self = Self {
            hue: 74,
            chroma: 65,
            hue2: 240,
            chroma2: 65,
            light: 67,
            weight: 155,
            tip: 20,
            dials: 0,
            speed: 100,
            still: 5,
            dance: 0,
            rest: 3,
            hours24: true,
            pace: 60,
            offset: 0,
            dark: 0,
            fade: 15,
            calm: false,
    };

    /// The stored size. Older, shorter forms still load: what they do not
    /// have comes from [`ClockConfig::DEFAULT`].
    pub const BYTES: usize = 31;
    /// The shortest form that was ever stored.
    const BYTES_MIN: usize = 23;

    /// The same settings, each within the range the Studio's patch allows.
    #[must_use]
    pub fn sanitised(self) -> Self {
        Self {
            hue: self.hue.min(360),
            chroma: self.chroma.min(200),
            hue2: self.hue2.min(360),
            chroma2: self.chroma2.min(200),
            light: self.light.clamp(40, 95),
            weight: self.weight.clamp(100, 240),
            tip: self.tip.clamp(20, 100),
            dials: self.dials.min(100),
            speed: self.speed.clamp(30, 360),
            still: self.still.clamp(3, 60),
            dance: self.dance,
            rest: self.rest,
            hours24: self.hours24,
            pace: self.pace.clamp(5, 60),
            offset: self.offset.min(1439),
            dark: self.dark,
            fade: self.fade.clamp(2, 50),
            calm: self.calm,
        }
    }

    /// The stored form: little-endian, in field order.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::BYTES] {
        let mut out = [0u8; Self::BYTES];
        let words = [
            self.hue,
            self.chroma,
            self.hue2,
            self.chroma2,
            self.light,
            self.weight,
            self.tip,
            self.dials,
            self.speed,
            self.still,
        ];
        for (chunk, w) in out.chunks_exact_mut(2).zip(words) {
            chunk.copy_from_slice(&w.to_le_bytes());
        }
        out[20] = self.dance;
        out[21] = self.rest;
        out[22] = u8::from(self.hours24);
        out[23..25].copy_from_slice(&self.pace.to_le_bytes());
        out[25..27].copy_from_slice(&self.offset.to_le_bytes());
        out[27] = self.dark;
        out[28..30].copy_from_slice(&self.fade.to_le_bytes());
        out[30] = u8::from(self.calm);
        out
    }

    /// Read the stored form back; `None` when it is not this shape.
    #[must_use]
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if !(Self::BYTES_MIN..=Self::BYTES).contains(&b.len()) {
            return None;
        }
        let d = Self::DEFAULT;
        let word = |i: usize, or: u16| b.get(i..i + 2).map_or(or, |w| u16::from_le_bytes([w[0], w[1]]));
        let w = |i: usize| u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]);
        Some(
            Self {
                hue: w(0),
                chroma: w(1),
                hue2: w(2),
                chroma2: w(3),
                light: w(4),
                weight: w(5),
                tip: w(6),
                dials: w(7),
                speed: w(8),
                still: w(9),
                dance: b[20],
                rest: b[21],
                hours24: b[22] != 0,
                pace: word(23, d.pace),
                offset: word(25, d.offset),
                dark: b.get(27).copied().unwrap_or(d.dark),
                fade: word(28, d.fade),
                calm: b.get(30).is_some_and(|v| *v != 0),
            }
            .sanitised(),
        )
    }
}
