//! The golden-file harness and the worst-case values, shared by the test
//! binaries.
//!
//! Each golden example is checked four ways:
//!
//! 1. `serde_json::to_string_pretty` of the value **is** the file, byte for
//!    byte. The files are pretty-printed so that they are worth reading - they
//!    are this API's documentation for the Studio side - and the compact form
//!    is checked separately in (4).
//! 2. `serde_json::from_str` of the file **is** the value. That is the Studio's
//!    parser.
//! 3. `serde_json_core::from_slice` of the file is the value too. That is the
//!    firmware's parser, the one picoserve's `Json` extractor uses.
//! 4. The compact forms from `serde_json` and from `serde_json_core::to_slice`
//!    are byte-identical, and that is what actually goes on the wire.
//!
//! Set `UPDATE_GOLDEN=1` to rewrite the files from the values.

#![allow(dead_code)]

use std::fmt::Debug;
use std::path::PathBuf;

use serde::{de::DeserializeOwned, Serialize};

use screeny_device_api::enums::{
    Accepted, FailReason, FirmwareError, FwSlot, FwState, IdleMode, ResetReason, StreamState,
    WifiState,
};
use screeny_device_api::error::{ErrorCode, ErrorReply};
use screeny_device_api::reply::{
    AcceptedReply, FirmwareReply, NetworksReply, PanicRecord, SettingsReply, StatusReply,
    TelemetryReply, WifiReply, MAX_NETWORKS,
};
use screeny_device_api::request::{IdentifyRequest, RebootRequest, SettingsRequest};

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"))
}

/// Check one value against `tests/golden/<name>.json`, all four ways.
pub fn check<T>(name: &str, value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let path = golden_path(name);
    let pretty = serde_json::to_string_pretty(value).expect("serde_json pretty") + "\n";

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &pretty).unwrap();
    }

    let golden = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display()));

    // 1. the file is what serde_json writes
    assert_eq!(pretty, golden, "{name}: serialised form changed");

    // The three writers agree byte for byte only when no string in the value
    // contains a `/` (picoserve escapes it) or a control character
    // (serde-json-core spells `\u00XX` in upper case). Keeping the goldens
    // clear of both is what makes them the truth for all three.
    assert!(
        !golden.contains('/'),
        "{name}: a golden must not contain '/': picoserve would write it as '\\/'"
    );
    assert!(
        !golden.contains("\\u"),
        "{name}: a golden must not contain a \\u escape: serde-json-core spells it in upper case"
    );

    // 2. serde_json reads it back
    let back: T = serde_json::from_str(&golden).expect("serde_json parse");
    assert_eq!(&back, value, "{name}: serde_json round trip");

    // 3. serde-json-core reads it back, from the pretty form, whitespace and
    //    all - which is what a hand-written `curl -d @file` sends.
    //    `from_slice_escaped`, never `from_slice`: see MIN_UNESCAPE_BUFFER.
    let mut unescape = [0u8; screeny_device_api::request::MIN_UNESCAPE_BUFFER];
    let (back, _used) = serde_json_core::from_slice_escaped::<T>(golden.as_bytes(), &mut unescape)
        .expect("serde-json-core parse");
    assert_eq!(&back, value, "{name}: serde-json-core round trip");

    // 4. the compact wire forms agree
    let compact = serde_json::to_string(value).expect("serde_json compact");
    let n = to_slice(value);
    assert_eq!(
        compact.as_bytes(),
        &SCRATCH.with(|s| s.borrow()[..n].to_vec())[..],
        "{name}: serde_json and serde-json-core disagree on the wire form"
    );
}

thread_local! {
    static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Serialise with `serde-json-core` into a scratch buffer and return the
/// length. The bytes are in `SCRATCH`.
fn to_slice<T: Serialize>(value: &T) -> usize {
    SCRATCH.with(|s| {
        let mut buf = s.borrow_mut();
        buf.resize(16 * 1024, 0);
        serde_json_core::to_slice(value, &mut buf).expect("serde-json-core")
    })
}

/// The compact wire bytes, from `serde_json` and from `serde-json-core`,
/// asserted to be the same.
///
/// Only valid for values whose strings hold no control character: the two
/// writers spell `\u00XX` in different cases (see [`wire_len`]).
pub fn wire<T: Serialize>(value: &T) -> Vec<u8> {
    let compact = serde_json::to_string(value).expect("serde_json");
    let n = to_slice(value);
    let core = SCRATCH.with(|s| s.borrow()[..n].to_vec());
    assert_eq!(
        compact.as_bytes(),
        &core[..],
        "the two writers disagree: serde_json {compact:?}"
    );
    core
}

/// The length of the compact wire form, asserted to be the same from both
/// writers.
///
/// Lengths always agree even where the bytes do not: the only differences
/// between the three writers are the case of a `\u00XX` escape (same length)
/// and picoserve's `\/` for `/` (one byte longer, and this crate's size bounds
/// allow six bytes for every byte of text, so they cover it).
pub fn wire_len<T: Serialize>(value: &T) -> usize {
    let json = serde_json::to_string(value).expect("serde_json").len();
    let core = to_slice(value);
    assert_eq!(json, core, "the two writers disagree on length");
    core
}

// ---------------------------------------------------------------------------
// Worst-case values
// ---------------------------------------------------------------------------

/// `N` bytes of `\u{1f}`, the character that costs the most to escape: one
/// byte in, `\u001f` (six characters) out, in all three writers.
pub fn worst_text<const N: usize>() -> heapless::String<N> {
    let mut s = heapless::String::new();
    for _ in 0..N {
        s.push('\u{1f}').unwrap();
    }
    assert_eq!(s.len(), N);
    s
}

/// The longest [`StatusReply`] that can exist.
pub fn worst_status() -> StatusReply {
    StatusReply {
        api: u8::MAX,
        id: worst_text(),
        name: worst_text(),
        fw: worst_text(),
        boot_id: u32::MAX,
        uptime_ms: u32::MAX,
        heap_used: u32::MAX,
        heap_size: u32::MAX,
        stack_free: u32::MAX,
        rssi_dbm: i8::MIN,
        brightness: u8::MAX,
        idle_mode: IdleMode::HoldForever,
        wifi_state: WifiState::Disconnected,
        ssid: Some(worst_text()),
        ip: Some(worst_text()),
        state: StreamState::Provisioning,
        portal: false,
        fw_slot: FwSlot::Unknown,
        fw_state: FwState::PendingVerify,
        reset_reason: ResetReason::DeepSleep,
        store_errors: u32::MAX,
        boot_count: u32::MAX,
        panic_count: u32::MAX,
        // `Some`, not `None`: the bound has to cover the reply that carries a
        // panic record, since that is the longest one that can exist.
        last_panic: Some(worst_panic()),
    }
}

/// The longest [`PanicRecord`] that can exist.
pub fn worst_panic() -> PanicRecord {
    PanicRecord {
        uptime_ms: u32::MAX,
        boot: u32::MAX,
        file: worst_text(),
        line: u32::MAX,
        consecutive: u32::MAX,
    }
}

/// The longest [`TelemetryReply`] that can exist.
pub fn worst_telemetry() -> TelemetryReply {
    TelemetryReply {
        uptime_ms: u32::MAX,
        frames_rx: u32::MAX,
        frames_shown: u32::MAX,
        frames_dropped_stale: u32::MAX,
        frames_dropped_superseded: u32::MAX,
        frames_dropped_decode: u32::MAX,
        frames_rejected: u32::MAX,
        seq_gaps: u32::MAX,
        interarrival_us: u16::MAX,
        jitter_us: u16::MAX,
        interarrival_max_us: u16::MAX,
        decode_us: u16::MAX,
        decode_us_max: u16::MAX,
        render_us_max: u16::MAX,
        rssi_dbm: i8::MIN,
        brightness: u8::MAX,
        state: u8::MAX,
        last_codec: u8::MAX,
    }
}

/// [`MAX_NETWORKS`] entries, each with the longest name that can escape the
/// furthest.
pub fn worst_networks() -> NetworksReply {
    let mut r = NetworksReply::new();
    for _ in 0..MAX_NETWORKS {
        let name = worst_text::<{ screeny_device_api::text::MAX_SSID_LEN }>();
        assert!(r.offer(name.as_bytes(), i8::MIN, false));
    }
    assert_eq!(r.networks.len(), MAX_NETWORKS);
    r
}

/// The longest [`WifiReply`] that can exist.
pub fn worst_wifi() -> WifiReply {
    WifiReply {
        state: WifiState::Disconnected,
        ssid: Some(worst_text()),
        ip: Some(worst_text()),
        reason: Some(FailReason::NotFound),
    }
}

/// The longest [`SettingsReply`] that can exist.
pub fn worst_settings_reply() -> SettingsReply {
    SettingsReply {
        name: worst_text(),
        brightness: u8::MAX,
        idle_mode: IdleMode::HoldForever,
    }
}

/// The longest [`FirmwareReply`] that can exist.
pub fn worst_firmware() -> FirmwareReply {
    FirmwareReply::failed(u32::MAX, FirmwareError::WrongProject)
}

/// The longest [`AcceptedReply`] that can exist.
pub fn worst_accepted() -> AcceptedReply {
    AcceptedReply {
        result: Accepted::Identifying,
    }
}

/// The longest [`ErrorReply`] that can exist.
pub fn worst_error() -> ErrorReply {
    ErrorReply {
        error: ErrorCode::MethodNotAllowed,
        detail: Some(worst_text()),
    }
}

/// The longest [`SettingsRequest`] that can exist.
pub fn worst_settings_request() -> SettingsRequest {
    SettingsRequest {
        name: Some(worst_text()),
        brightness: Some(u8::MAX),
        idle_mode: Some(IdleMode::HoldForever),
        pin: Some(worst_text()),
        counter: Some(u32::MAX),
    }
}

/// The longest [`RebootRequest`] that can exist.
pub fn worst_reboot_request() -> RebootRequest {
    RebootRequest {
        confirm: worst_text(),
        pin: Some(worst_text()),
        counter: Some(u32::MAX),
    }
}

/// The longest [`IdentifyRequest`] that can exist.
pub fn worst_identify_request() -> IdentifyRequest {
    IdentifyRequest {
        duration_ms: u32::MAX,
        pin: Some(worst_text()),
        counter: Some(u32::MAX),
    }
}
