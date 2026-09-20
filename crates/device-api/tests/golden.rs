//! One checked-in example of every request and every reply.
//!
//! These files are the API's documentation for the Studio side, and they are
//! load-bearing: change a field name, a field order or an enum spelling and
//! this test fails with the diff. See `tests/common/mod.rs` for what "check"
//! means.

mod common;

use common::check;
use screeny_device_api::enums::{
    Accepted, FailReason, FirmwareError, FwSlot, FwState, IdleMode, ResetReason, StreamState,
    WifiState,
};
use screeny_device_api::error::{ErrorCode, ErrorReply};
use screeny_device_api::reply::{
    AcceptedReply, FirmwareReply, NetworksReply, PanicRecord, SettingsReply, StatusReply,
    TelemetryReply, WifiReply,
};
use screeny_device_api::request::{IdentifyRequest, RebootRequest, SettingsRequest};
use screeny_device_api::text::{ipv4_text, text};
use screeny_device_api::API_VERSION;

/// The device on the bench, online and showing a stream.
fn status() -> StatusReply {
    StatusReply {
        api: API_VERSION,
        id: text("4a00a4").unwrap(),
        name: text("Studio panel").unwrap(),
        fw: text("0.2.0").unwrap(),
        boot_id: 3_054_198_966,
        uptime_ms: 3_600_000,
        heap_used: 46_112,
        heap_size: 98_304,
        stack_free: 14_016,
        rssi_dbm: -54,
        brightness: 96,
        idle_mode: IdleMode::Status,
        wifi_state: WifiState::Connected,
        ssid: text("Example-Wifi1"),
        ip: Some(ipv4_text([192, 168, 7, 221])),
        state: StreamState::Live,
        portal: false,
        fw_slot: FwSlot::Ota0,
        fw_state: FwState::Valid,
        reset_reason: ResetReason::PowerOn,
        store_errors: 0,
        boot_count: 1,
        panic_count: 0,
        last_panic: None,
    }
}

#[test]
fn status_online() {
    check("status", &status());
}

#[test]
fn status_after_a_panic() {
    // Card 243: the same device, having panicked once and rebooted by itself.
    // `reset_reason` is `software` because that is all the ESP32's register
    // says about a panic; the breadcrumb is what makes it a panic, and says
    // where. This is the shape the Studio reads to know a panel restarted for
    // a reason rather than because somebody pulled the plug.
    let s = StatusReply {
        uptime_ms: 42_000,
        boot_id: 2_244_121_910,
        reset_reason: ResetReason::Software,
        boot_count: 2,
        panic_count: 1,
        last_panic: Some(PanicRecord {
            uptime_ms: 94_312,
            boot: 1,
            file: text("net.rs").unwrap(),
            line: 321,
            consecutive: 1,
        }),
        ..status()
    };
    check("status_panicked", &s);
}

#[test]
fn status_in_the_portal() {
    // Nothing joined, the setup AP is up, the panel is showing the QR, and the
    // image that is running has not been confirmed healthy yet.
    let s = StatusReply {
        name: text("").unwrap(),
        boot_id: 1_194_684,
        uptime_ms: 41_500,
        rssi_dbm: 0,
        wifi_state: WifiState::Disconnected,
        ssid: None,
        ip: None,
        state: StreamState::Provisioning,
        portal: true,
        fw_slot: FwSlot::Ota1,
        fw_state: FwState::PendingVerify,
        reset_reason: ResetReason::Software,
        store_errors: 2,
        boot_count: 3,
        ..status()
    };
    check("status_portal", &s);
}

#[test]
fn telemetry() {
    // Spec 6.7's 48 bytes, through `From<&Telemetry>`, so the golden is the
    // wire struct's own numbers and not a second transcription of them.
    let wire = screeny_proto::control::Telemetry {
        uptime_ms: 3_600_000,
        frames_rx: 108_000,
        frames_shown: 107_912,
        frames_dropped_stale: 41,
        frames_dropped_superseded: 39,
        frames_dropped_decode: 8,
        frames_rejected: 0,
        seq_gaps: 12,
        interarrival_us: 33_341,
        jitter_us: 412,
        interarrival_max_us: 61_200,
        decode_us: 1_870,
        decode_us_max: 4_120,
        render_us_max: 980,
        rssi_dbm: -54,
        brightness: 96,
        state: 1,
        last_codec: 3,
    };
    check("telemetry", &TelemetryReply::from(&wire));
}

#[test]
fn networks() {
    let mut r = NetworksReply::new();
    // Offered in any order; the reply is strongest first by construction.
    assert!(r.offer("Caf\u{e9} du coin".as_bytes(), -71, true));
    assert!(r.offer(b"Example-Wifi1", -54, true));
    assert!(r.offer(b"guest", -66, false));
    check("networks", &r);
}

#[test]
fn wifi_states() {
    check("wifi_connected", &{
        WifiReply {
            state: WifiState::Connected,
            ssid: text("Example-Wifi1"),
            ip: Some(ipv4_text([192, 168, 7, 221])),
            reason: None,
        }
    });
    check("wifi_disconnected", &WifiReply::disconnected());
    check(
        "wifi_failed",
        &WifiReply {
            state: WifiState::Failed,
            ssid: text("Example-Wifi1"),
            ip: None,
            reason: Some(FailReason::Auth),
        },
    );
    check(
        "wifi_trying",
        &WifiReply {
            state: WifiState::Connecting,
            ssid: text("Example-Wifi1"),
            ip: None,
            reason: None,
        },
    );
}

#[test]
fn settings() {
    check(
        "settings_request",
        &SettingsRequest {
            name: text("Studio panel"),
            brightness: Some(96),
            idle_mode: Some(IdleMode::Dim),
            pin: None,
            counter: None,
        },
    );
    // A brightness slider, which knows nothing about the other two settings.
    check(
        "settings_request_brightness_only",
        &SettingsRequest {
            brightness: Some(200),
            ..SettingsRequest::default()
        },
    );
    // The reply is the whole state, with the firmware's cap already applied:
    // 200 asked for, 128 in effect.
    check(
        "settings_reply",
        &SettingsReply {
            name: text("Studio panel").unwrap(),
            brightness: 128,
            idle_mode: IdleMode::Dim,
        },
    );
}

#[test]
fn firmware() {
    check("firmware_ok", &FirmwareReply::ok(761_232));
    check(
        "firmware_failed",
        &FirmwareReply::failed(4_096, FirmwareError::WrongProject),
    );
}

#[test]
fn accepted() {
    check("accepted_trying", &AcceptedReply::TRYING);
    check("accepted_rebooting", &AcceptedReply::REBOOTING);
    check("accepted_identifying", &AcceptedReply::IDENTIFYING);
}

#[test]
fn reboot_and_identify() {
    check(
        "reboot_request",
        &RebootRequest {
            confirm: text("RBOO").unwrap(),
            pin: None,
            counter: None,
        },
    );
    check(
        "identify_request",
        &IdentifyRequest {
            duration_ms: 10_000,
            pin: None,
            counter: None,
        },
    );
    // What a request looks like once parked card 041 gives the PIN meaning.
    // It parses today and is ignored today; the shape does not change then.
    check(
        "identify_request_with_pin",
        &IdentifyRequest {
            duration_ms: 10_000,
            pin: text("2468"),
            counter: Some(17),
        },
    );
}

#[test]
fn errors() {
    check("error", &ErrorReply::new(ErrorCode::Busy));
    check(
        "error_with_detail",
        &ErrorReply::with_detail(ErrorCode::OutOfRange, "ssid is longer than 32 bytes"),
    );
}

#[test]
fn every_accepted_result_has_a_golden() {
    // A new variant of a closed set without a golden file is the exact drift
    // this crate exists to stop, so the sets that are cheap to enumerate are
    // enumerated.
    for (a, name) in [
        (Accepted::Trying, "accepted_trying"),
        (Accepted::Rebooting, "accepted_rebooting"),
        (Accepted::Identifying, "accepted_identifying"),
    ] {
        check(name, &AcceptedReply { result: a });
    }
}

#[test]
fn the_golden_directory_has_no_strays() {
    // A file nobody checks is a file that is quietly wrong. Every golden must
    // have been written by one of the tests above in the same run, so this
    // test just makes sure the directory is not accumulating orphans: it
    // lists what is there and fails on a name no test mentions.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let known: &[&str] = &[
        "status",
        "status_portal",
        "status_panicked",
        "telemetry",
        "networks",
        "wifi_connected",
        "wifi_disconnected",
        "wifi_failed",
        "wifi_trying",
        "settings_request",
        "settings_request_brightness_only",
        "settings_reply",
        "firmware_ok",
        "firmware_failed",
        "accepted_trying",
        "accepted_rebooting",
        "accepted_identifying",
        "reboot_request",
        "identify_request",
        "identify_request_with_pin",
        "error",
        "error_with_detail",
    ];
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        // `wifi_form.txt` is the urlencoded body, not JSON; `tests/form.rs`
        // owns it.
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    found.sort();
    let mut expected: Vec<String> = known.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    assert_eq!(found, expected);
}
