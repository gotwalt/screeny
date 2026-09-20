//! `MAX_JSON_LEN` is a promise; this is the proof.
//!
//! Every request and reply type gets its longest possible value serialised,
//! by both writers, and the length compared with the constant. The comparison
//! is **equality**, not `<=`: a bound that is merely an over-estimate rots
//! quietly, and card 222 sizes buffers from these numbers. A field added
//! without extending the bound fails here.

mod common;

use common::*;
use screeny_device_api::error::ErrorReply;
use screeny_device_api::form::MAX_FORM_LEN;
use screeny_device_api::reply::{
    AcceptedReply, FirmwareReply, NetworksReply, PanicReply, SettingsReply, StatusReply,
    TelemetryReply, WifiReply,
};
use screeny_device_api::request::{IdentifyRequest, RebootRequest, SettingsRequest};
use screeny_device_api::route::{Body, MAX_REQUEST_LEN, ROUTES};

use serde::Serialize;

#[track_caller]
fn exactly<T: Serialize>(what: &str, value: &T, bound: usize) {
    let len = wire_len(value);
    assert_eq!(
        len, bound,
        "{what}: worst case is {len} bytes, MAX_JSON_LEN says {bound}"
    );
}

#[test]
fn every_reply_bound_is_exact() {
    exactly("StatusReply", &worst_status(), StatusReply::MAX_JSON_LEN);
    exactly(
        "TelemetryReply",
        &worst_telemetry(),
        TelemetryReply::MAX_JSON_LEN,
    );
    exactly(
        "NetworksReply",
        &worst_networks(),
        NetworksReply::MAX_JSON_LEN,
    );
    exactly("WifiReply", &worst_wifi(), WifiReply::MAX_JSON_LEN);
    exactly("PanicReply", &worst_panic_reply(), PanicReply::MAX_JSON_LEN);
    exactly(
        "SettingsReply",
        &worst_settings_reply(),
        SettingsReply::MAX_JSON_LEN,
    );
    exactly(
        "FirmwareReply",
        &worst_firmware(),
        FirmwareReply::MAX_JSON_LEN,
    );
    exactly(
        "AcceptedReply",
        &worst_accepted(),
        AcceptedReply::MAX_JSON_LEN,
    );
    exactly("ErrorReply", &worst_error(), ErrorReply::MAX_JSON_LEN);
}

#[test]
fn every_request_bound_is_exact() {
    exactly(
        "SettingsRequest",
        &worst_settings_request(),
        SettingsRequest::MAX_JSON_LEN,
    );
    exactly(
        "RebootRequest",
        &worst_reboot_request(),
        RebootRequest::MAX_JSON_LEN,
    );
    exactly(
        "IdentifyRequest",
        &worst_identify_request(),
        IdentifyRequest::MAX_JSON_LEN,
    );
}

#[test]
fn a_typical_reply_is_far_shorter_than_its_bound() {
    // Not a requirement, a sanity check with a number in it: the bounds above
    // assume every byte of every name escapes to six characters, which never
    // happens, so nobody should read `MAX_JSON_LEN` as "what a status reply
    // costs on the wire". The real one, with a name and an SSID somebody
    // would actually type, is 364 bytes against a bound of 1018. **Card 243
    // took its breadcrumb fields back out of this type**: 44 bytes added here
    // cost 3,488 bytes of the device's stack, because the firmware moves one
    // of these through picoserve's response chain many times in one inlined
    // async frame. The breadcrumb is `PanicReply` on its own route.
    let real = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/status.json"
    ))
    .unwrap();
    let value: StatusReply = serde_json::from_str(&real).unwrap();
    let on_the_wire = wire(&value).len();
    assert!(
        on_the_wire < 400,
        "a real status reply is {on_the_wire} bytes"
    );
    assert!(
        StatusReply::MAX_JSON_LEN > on_the_wire * 2,
        "the bound is {} and a real reply is {on_the_wire}",
        StatusReply::MAX_JSON_LEN
    );
}

#[test]
fn the_route_table_quotes_the_same_numbers() {
    // The table card 222 reads must not drift from the constants.
    for r in ROUTES {
        match r.body {
            Body::None | Body::Stream => assert_eq!(r.max_request_len, 0, "{}", r.path),
            Body::Form => assert_eq!(r.max_request_len, MAX_FORM_LEN, "{}", r.path),
            Body::Json => assert!(r.max_request_len > 0, "{}", r.path),
        }
        assert!(r.max_reply_len > 0, "{}", r.path);
        assert!(r.max_request_len <= MAX_REQUEST_LEN, "{}", r.path);
    }
}

#[test]
fn the_numbers_card_222_needs_are_printed_here() {
    // Not an assertion so much as a place to read them off: `cargo test -p
    // screeny-device-api -- --nocapture the_numbers` prints the table.
    println!("MAX_REQUEST_LEN (sizes picoserve's HTTP buffer) = {MAX_REQUEST_LEN}");
    for r in ROUTES {
        println!(
            "{:?} {:24} request<={:5} reply<={:5} {:?}",
            r.method, r.path, r.max_request_len, r.max_reply_len, r.body
        );
    }
    assert_eq!(MAX_REQUEST_LEN, MAX_FORM_LEN);
}

/// Card 243b's lesson, as a test: **a reply type is paid many times over in
/// the firmware's request path**, because one of these is moved through
/// picoserve's response chain repeatedly inside a single inlined async frame.
/// 44 bytes added to `StatusReply` cost 3,488 bytes of core 0's stack on the
/// bench.
///
/// The firmware's `ApiBody` enum is as big as its biggest variant, so the
/// question any new reply type has to answer is "is it bigger than
/// `StatusReply`?". This writes the answers down. It is not a budget - a card
/// that needs a bigger reply can have one and pay for it - it is the number to
/// look at before deciding that. Card 240 added `FirmwareReply` and this is
/// where it was checked.
#[test]
fn no_reply_is_bigger_than_the_one_on_the_hot_path() {
    use core::mem::size_of;
    let status = size_of::<StatusReply>();
    println!("StatusReply = {status} bytes (the hot path, GET /api/v1/status)");
    for (name, n) in [
        ("TelemetryReply", size_of::<TelemetryReply>()),
        ("PanicReply", size_of::<PanicReply>()),
        ("WifiReply", size_of::<WifiReply>()),
        ("SettingsReply", size_of::<SettingsReply>()),
        ("AcceptedReply", size_of::<AcceptedReply>()),
        ("FirmwareReply", size_of::<FirmwareReply>()),
    ] {
        println!("{name} = {n} bytes");
        assert!(
            n <= status,
            "{name} is {n} bytes against StatusReply's {status}: it, and not status, \
             now sets the size of the firmware's reply enum and of every frame that \
             holds one. That may be the right trade - card 243b priced it at about \
             12x in the request path - but it is a decision, so make it here."
        );
    }
    // `NetworksReply` is the exception and is left out on purpose: it holds 16
    // entries and is far bigger than any of these. The firmware does not serve
    // it (device-web decision 10 dropped card 229) and its reply enum has no
    // variant for it, which is why this list is explicit rather than "every
    // type in the module".
    println!(
        "NetworksReply = {} bytes (not served by the firmware)",
        size_of::<NetworksReply>()
    );
}
