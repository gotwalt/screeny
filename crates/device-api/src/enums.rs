//! Every closed set of strings the API uses.
//!
//! These are enums rather than `&str` on purpose: a firmware that grows a new
//! idle mode and a Studio that has not heard of it will fail to parse loudly,
//! in one place, instead of comparing strings in five.
//!
//! Where the same set already exists as a byte in `screeny-proto` or as a
//! `&'static str` in `screeny-provision`, the conversion is here and the other
//! crate is not touched.

use screeny_proto::control::{self, IdleMode as ProtoIdleMode};
use serde::{Deserialize, Serialize};

/// The longest variant name in a set, used by the `MAX_JSON_LEN` arithmetic.
macro_rules! longest {
    ($($s:expr),+ $(,)?) => {{
        let mut n = 0;
        $( if $s.len() > n { n = $s.len(); } )+
        n
    }};
}

/// What the panel does when nothing is streaming. Spec section 6.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleMode {
    /// The status screen.
    Status,
    /// Keep showing the last frame forever.
    HoldForever,
    /// Dim the last frame.
    Dim,
    /// Black.
    Black,
}

impl IdleMode {
    /// The longest serialised form, quotes included.
    pub(crate) const MAX_JSON_LEN: usize =
        2 + longest!("status", "hold_forever", "dim", "black");
}

impl From<ProtoIdleMode> for IdleMode {
    fn from(m: ProtoIdleMode) -> Self {
        match m {
            ProtoIdleMode::Status => IdleMode::Status,
            ProtoIdleMode::HoldForever => IdleMode::HoldForever,
            ProtoIdleMode::Dim => IdleMode::Dim,
            ProtoIdleMode::Black => IdleMode::Black,
        }
    }
}

impl From<IdleMode> for ProtoIdleMode {
    fn from(m: IdleMode) -> Self {
        match m {
            IdleMode::Status => ProtoIdleMode::Status,
            IdleMode::HoldForever => ProtoIdleMode::HoldForever,
            IdleMode::Dim => ProtoIdleMode::Dim,
            IdleMode::Black => ProtoIdleMode::Black,
        }
    }
}

/// The station's join state: the `GET_WIFI` byte of spec section 6.3.
///
/// (Section 8.3 is the *join sequence* - what the device tries and in what
/// order. The byte itself is in the opcode table.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiState {
    /// No credentials, or the radio is down. A button wipe lands here, not in
    /// [`WifiState::Failed`] (card 221).
    Disconnected,
    /// Associating, or waiting for DHCP.
    Connecting,
    /// Joined, with an address.
    Connected,
    /// The last attempt failed. See [`FailReason`].
    Failed,
}

impl WifiState {
    pub(crate) const MAX_JSON_LEN: usize =
        2 + longest!("disconnected", "connecting", "connected", "failed");

    /// The [`screeny_proto::control::wifi_state`] byte.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            WifiState::Disconnected => control::wifi_state::DISCONNECTED,
            WifiState::Connecting => control::wifi_state::CONNECTING,
            WifiState::Connected => control::wifi_state::CONNECTED,
            WifiState::Failed => control::wifi_state::FAILED,
        }
    }

    /// Read a [`screeny_proto::control::wifi_state`] byte; `None` if the
    /// device reported one this build does not know.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            control::wifi_state::DISCONNECTED => WifiState::Disconnected,
            control::wifi_state::CONNECTING => WifiState::Connecting,
            control::wifi_state::CONNECTED => WifiState::Connected,
            control::wifi_state::FAILED => WifiState::Failed,
            _ => return None,
        })
    }
}

/// The stream state: the `state` byte of telemetry, spec section 6.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamState {
    /// Nothing is streaming.
    Idle,
    /// A source holds the lock and frames are arriving.
    Live,
    /// Holding the last frame.
    Hold,
    /// Showing the identify screen.
    Identify,
    /// The portal or a trial join is up.
    Provisioning,
}

impl StreamState {
    pub(crate) const MAX_JSON_LEN: usize =
        2 + longest!("idle", "live", "hold", "identify", "provisioning");

    /// The [`screeny_proto::control::state`] byte.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            StreamState::Idle => control::state::IDLE,
            StreamState::Live => control::state::LIVE,
            StreamState::Hold => control::state::HOLD,
            StreamState::Identify => control::state::IDENTIFY,
            StreamState::Provisioning => control::state::PROVISIONING,
        }
    }

    /// Read a [`screeny_proto::control::state`] byte; `None` if unknown here.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            control::state::IDLE => StreamState::Idle,
            control::state::LIVE => StreamState::Live,
            control::state::HOLD => StreamState::Hold,
            control::state::IDENTIFY => StreamState::Identify,
            control::state::PROVISIONING => StreamState::Provisioning,
            _ => return None,
        })
    }
}

/// Why the last join attempt failed. These three strings are
/// [`screeny_provision::machine::FailReason::as_str`]'s, and there is a test
/// that says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    /// Wrong password.
    Auth,
    /// No AP with that SSID was found.
    NotFound,
    /// Anything else, including the attempt timeout.
    Other,
}

impl FailReason {
    pub(crate) const MAX_JSON_LEN: usize = 2 + longest!("auth", "not_found", "other");
}

impl From<screeny_provision::machine::FailReason> for FailReason {
    fn from(r: screeny_provision::machine::FailReason) -> Self {
        use screeny_provision::machine::FailReason as P;
        match r {
            P::AuthError => FailReason::Auth,
            P::NetworkNotFound => FailReason::NotFound,
            P::Other => FailReason::Other,
        }
    }
}

/// Which app slot is running. Research 006: `ota_0` at 0x10000, `ota_1` at
/// 0x210000, and no `factory` partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FwSlot {
    /// The first app slot.
    #[serde(rename = "ota_0")]
    Ota0,
    /// The second app slot.
    #[serde(rename = "ota_1")]
    Ota1,
    /// Running from something else, or the running partition could not be
    /// read. A build flashed before card 210's partition table landed reports
    /// this rather than lying.
    #[serde(rename = "unknown")]
    Unknown,
}

impl FwSlot {
    pub(crate) const MAX_JSON_LEN: usize = 2 + longest!("ota_0", "ota_1", "unknown");
}

/// The running slot's `otadata` state. The names are ESP-IDF's
/// `esp_ota_img_states_t`, lower-cased.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FwState {
    /// Written, never booted.
    New,
    /// Booted once and on trial; the health criterion has not passed yet.
    PendingVerify,
    /// Confirmed healthy.
    Valid,
    /// Marked bad; will not be booted again.
    Invalid,
    /// Rolled back.
    Aborted,
    /// No `otadata` entry, or a bootloader without rollback support.
    Undefined,
}

impl FwState {
    pub(crate) const MAX_JSON_LEN: usize = 2 + longest!(
        "new",
        "pending_verify",
        "valid",
        "invalid",
        "aborted",
        "undefined",
    );
}

/// Why the chip last restarted. The ESP32's `RESET_REASON`, named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetReason {
    /// Power applied.
    PowerOn,
    /// The external reset pin, or `espflash` toggling it.
    External,
    /// A software reset: `esp_restart`, or our own `REBOOT`.
    Software,
    /// A panic or an unhandled exception.
    Panic,
    /// The interrupt watchdog.
    IntWdt,
    /// A task watchdog.
    TaskWdt,
    /// Another watchdog (RTC, group).
    Wdt,
    /// Woken from deep sleep.
    DeepSleep,
    /// The brownout detector. On this bench that means the USB rail sagged.
    Brownout,
    /// Reset over SDIO.
    Sdio,
    /// The chip reported something this build does not name.
    Unknown,
}

impl ResetReason {
    pub(crate) const MAX_JSON_LEN: usize = 2 + longest!(
        "power_on",
        "external",
        "software",
        "panic",
        "int_wdt",
        "task_wdt",
        "wdt",
        "deep_sleep",
        "brownout",
        "sdio",
        "unknown",
    );
}

/// Why a firmware upload was refused.
///
/// The first five are research 006 section 5's five checks on the image
/// header, in the order the validator runs them; the last three are the sink
/// refusing before or during the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareError {
    /// The image does not start with the ESP32 image magic `0xE9`.
    BadMagic,
    /// The image header names a different chip.
    WrongChip,
    /// The app descriptor's project name is not `screeny-fw`.
    WrongProject,
    /// The one-byte XOR checksum at the end of the segments is wrong.
    BadChecksum,
    /// The appended SHA-256 does not match the image.
    BadSha256,
    /// Longer than the inactive slot.
    TooLarge,
    /// Another upload, or another flash write, is in progress.
    Busy,
    /// The flash write itself failed.
    Flash,
}

impl FirmwareError {
    pub(crate) const MAX_JSON_LEN: usize = 2 + longest!(
        "bad_magic",
        "wrong_chip",
        "wrong_project",
        "bad_checksum",
        "bad_sha256",
        "too_large",
        "busy",
        "flash",
    );
}

/// What a mutating route that did not fail is doing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accepted {
    /// `POST /api/v1/wifi`: the reply goes out **before** the radio work, the
    /// way `SET_WIFI` does (spec 8.2), because the join drops the connection
    /// that would carry it.
    Trying,
    /// `POST /api/v1/reboot`: sent before rebooting.
    Rebooting,
    /// `POST /api/v1/identify`: the screen is up.
    Identifying,
}

impl Accepted {
    pub(crate) const MAX_JSON_LEN: usize =
        2 + longest!("trying", "rebooting", "identifying");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;

    #[test]
    fn fail_reason_strings_are_the_provision_crates() {
        use screeny_provision::machine::FailReason as P;
        for (p, ours) in [
            (P::AuthError, FailReason::Auth),
            (P::NetworkNotFound, FailReason::NotFound),
            (P::Other, FailReason::Other),
        ] {
            assert_eq!(FailReason::from(p), ours);
            let json = serde_json::to_string(&ours).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
        }
    }

    #[test]
    fn idle_mode_round_trips_through_proto() {
        for m in [
            ProtoIdleMode::Status,
            ProtoIdleMode::HoldForever,
            ProtoIdleMode::Dim,
            ProtoIdleMode::Black,
        ] {
            assert_eq!(ProtoIdleMode::from(IdleMode::from(m)), m);
        }
    }

    #[test]
    fn state_bytes_round_trip() {
        for s in [
            StreamState::Idle,
            StreamState::Live,
            StreamState::Hold,
            StreamState::Identify,
            StreamState::Provisioning,
        ] {
            assert_eq!(StreamState::from_u8(s.as_u8()), Some(s));
        }
        assert_eq!(StreamState::from_u8(200), None);
        for s in [
            WifiState::Disconnected,
            WifiState::Connecting,
            WifiState::Connected,
            WifiState::Failed,
        ] {
            assert_eq!(WifiState::from_u8(s.as_u8()), Some(s));
        }
        assert_eq!(WifiState::from_u8(200), None);
    }
}
