//! Spec section 8.4: the PSK never appears in a reply, a log line or on the
//! panel. This is the reply half, checked rather than remembered.
//!
//! Every reply type is serialised at its worst case and the bytes are searched
//! for `psk` and `pass`. It is a crude test on purpose: it catches a field
//! called `psk`, one called `password`, one called `passphrase`, and a
//! `Debug` that let one through - without anybody having to notice.

mod common;

use common::*;
use screeny_device_api::form::parse_wifi_form;

/// The dummy credentials the whole repo uses. Never the real ones.
const SSID: &[u8] = b"Example-Wifi1";
const PSK: &[u8] = b"password9";

#[track_caller]
fn no_secret_words(what: &str, bytes: &[u8]) {
    let lower: Vec<u8> = bytes.iter().map(u8::to_ascii_lowercase).collect();
    // Not "key": a codec or a QR field could honestly be called one. These
    // four are the words a credential hides behind.
    for needle in [&b"psk"[..], b"pass", b"secret", b"credential"] {
        assert!(
            !lower.windows(needle.len()).any(|w| w == needle),
            "{what}: serialised form contains {:?}",
            core::str::from_utf8(needle).unwrap()
        );
    }
}

#[test]
fn no_reply_type_can_carry_a_psk() {
    no_secret_words("StatusReply", &serde_json::to_vec(&worst_status()).unwrap());
    no_secret_words(
        "TelemetryReply",
        &serde_json::to_vec(&worst_telemetry()).unwrap(),
    );
    no_secret_words(
        "NetworksReply",
        &serde_json::to_vec(&worst_networks()).unwrap(),
    );
    no_secret_words("WifiReply", &serde_json::to_vec(&worst_wifi()).unwrap());
    no_secret_words(
        "SettingsReply",
        &serde_json::to_vec(&worst_settings_reply()).unwrap(),
    );
    no_secret_words(
        "FirmwareReply",
        &serde_json::to_vec(&worst_firmware()).unwrap(),
    );
    no_secret_words(
        "AcceptedReply",
        &serde_json::to_vec(&worst_accepted()).unwrap(),
    );
    no_secret_words("ErrorReply", &serde_json::to_vec(&worst_error()).unwrap());
    no_secret_words(
        "PanicReply",
        &serde_json::to_vec(&worst_panic_reply()).unwrap(),
    );
}

#[test]
fn every_golden_file_is_free_of_them_too() {
    // The goldens are what a reviewer reads. If one ever grows a `psk` key,
    // it fails here as well as in the type.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            let body = std::fs::read(&path).unwrap();
            no_secret_words(&path.display().to_string(), &body);
        }
    }
}

#[test]
fn the_form_that_does_hold_one_never_prints_it() {
    // `WifiForm` is the one type in the crate that holds a PSK. It is a
    // request, it is never serialised, and its `Debug` prints the length.
    let mut body = Vec::new();
    body.extend_from_slice(b"ssid=");
    body.extend_from_slice(SSID);
    body.extend_from_slice(b"&psk=");
    body.extend_from_slice(PSK);
    let form = parse_wifi_form(&body).unwrap();
    assert_eq!(form.psk(), PSK);

    let debug = format!("{form:?}");
    assert!(!debug.contains("password9"), "{debug}");
    assert!(debug.contains("psk_len"), "{debug}");
    assert!(debug.contains(&format!("{}", PSK.len())), "{debug}");
    // The SSID is not a secret and is shown.
    assert!(debug.contains("Example-Wifi1"), "{debug}");
}

#[test]
fn a_psk_with_a_quote_in_it_still_does_not_leak_through_debug() {
    // Percent-encoded `"` and `\`, which is what would escape into a JSON
    // reply if one ever carried a PSK.
    let form = parse_wifi_form(b"ssid=Example-Wifi1&psk=a%22b%5Cc9").unwrap();
    assert_eq!(form.psk(), b"a\"b\\c9");
    let debug = format!("{form:?}");
    assert!(!debug.contains("a\"b"), "{debug}");
    assert!(debug.contains("psk_len: 6"), "{debug}");
}
