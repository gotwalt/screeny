//! `POST /api/v1/wifi`: the urlencoded body, every way it can arrive.
//!
//! Credentials pass through here once, from a phone standing in front of the
//! panel, and a wrong answer is a device that has to be reset with the button.
//! So: bytes not text, every escape form, both delimiters inside a password,
//! and a refusal for everything ambiguous.
//!
//! The example body lives in `tests/golden/wifi_form.txt`, beside the JSON
//! goldens, because the Studio and the simulator need to know what a browser
//! actually sends.

use screeny_device_api::error::ErrorCode;
use screeny_device_api::form::{parse_wifi_form, FormError, MAX_FORM_LEN};
use screeny_device_api::text::{MAX_PSK_LEN, MAX_SSID_LEN};

/// Never the real ones (CLAUDE.md).
const SSID: &str = "Example-Wifi1";
const PSK: &str = "password9";

#[test]
fn the_body_a_browser_sends() {
    let golden = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/wifi_form.txt"
    ))
    .unwrap();
    // The file ends with a newline for the sake of every editor; a real body
    // does not.
    let body = golden.strip_suffix(b"\n").unwrap_or(&golden);
    let form = parse_wifi_form(body).unwrap();
    assert_eq!(form.ssid(), SSID.as_bytes());
    assert_eq!(form.psk(), PSK.as_bytes());
    assert!(!form.is_open());
    assert_eq!(form.pin, None);
    assert_eq!(form.counter, None);
}

#[test]
fn an_empty_psk_is_an_open_network() {
    let form = parse_wifi_form(b"ssid=Example-Wifi1&psk=").unwrap();
    assert!(form.is_open());
    assert_eq!(form.psk_len(), 0);
    // ...and so is no `psk` key at all, for the sake of `curl -d ssid=...`.
    let form = parse_wifi_form(b"ssid=Example-Wifi1").unwrap();
    assert!(form.is_open());
}

#[test]
fn thirty_two_bytes_of_ssid_fit_and_thirty_three_do_not() {
    let ok = "a".repeat(MAX_SSID_LEN);
    let form = parse_wifi_form(format!("ssid={ok}&psk=").as_bytes()).unwrap();
    assert_eq!(form.ssid().len(), MAX_SSID_LEN);

    let too_long = "a".repeat(MAX_SSID_LEN + 1);
    assert_eq!(
        parse_wifi_form(format!("ssid={too_long}&psk=").as_bytes()),
        Err(FormError::SsidTooLong)
    );
    // The same limit applies after decoding, not before: 33 escaped bytes are
    // 99 characters of body and still one byte too many.
    let escaped: String = (0..=MAX_SSID_LEN).map(|_| "%61").collect();
    assert_eq!(
        parse_wifi_form(format!("ssid={escaped}").as_bytes()),
        Err(FormError::SsidTooLong)
    );
}

#[test]
fn sixty_four_bytes_of_psk_fit_and_sixty_five_do_not() {
    let ok = "p".repeat(MAX_PSK_LEN);
    let form = parse_wifi_form(format!("ssid=Example-Wifi1&psk={ok}").as_bytes()).unwrap();
    assert_eq!(form.psk_len(), MAX_PSK_LEN);

    let too_long = "p".repeat(MAX_PSK_LEN + 1);
    assert_eq!(
        parse_wifi_form(format!("ssid=Example-Wifi1&psk={too_long}").as_bytes()),
        Err(FormError::PskTooLong)
    );
}

#[test]
fn an_empty_ssid_is_refused() {
    assert_eq!(
        parse_wifi_form(b"ssid=&psk=password9"),
        Err(FormError::SsidEmpty)
    );
    assert_eq!(
        parse_wifi_form(b"psk=password9"),
        Err(FormError::MissingSsid)
    );
    assert_eq!(parse_wifi_form(b""), Err(FormError::MissingSsid));
}

#[test]
fn percent_encoded_utf8_survives_as_bytes() {
    // "Café du coin", as a browser encodes it.
    let form = parse_wifi_form(b"ssid=Caf%C3%A9%20du%20coin&psk=").unwrap();
    assert_eq!(form.ssid(), "Caf\u{e9} du coin".as_bytes());
    assert_eq!(
        core::str::from_utf8(form.ssid()).unwrap(),
        "Caf\u{e9} du coin"
    );
    // Lower-case hex too.
    let form = parse_wifi_form(b"ssid=Caf%c3%a9&psk=").unwrap();
    assert_eq!(form.ssid(), "Caf\u{e9}".as_bytes());
}

#[test]
fn an_ssid_that_is_not_utf8_at_all_is_still_accepted() {
    // This is the whole reason the parser works on bytes: 802.11 does not
    // require UTF-8 and some routers ship with a latin-1 name.
    let form = parse_wifi_form(b"ssid=%FF%FE%01&psk=").unwrap();
    assert_eq!(form.ssid(), &[0xff, 0xfe, 0x01]);
    assert!(core::str::from_utf8(form.ssid()).is_err());
    // ...and it has no JSON representation, which is the reply side's problem
    // and is handled there.
    assert_eq!(screeny_device_api::text::ssid_text(form.ssid()), None);
}

#[test]
fn plus_is_a_space_and_percent_twenty_is_too() {
    let a = parse_wifi_form(b"ssid=my+network&psk=").unwrap();
    let b = parse_wifi_form(b"ssid=my%20network&psk=").unwrap();
    assert_eq!(a.ssid(), b"my network");
    assert_eq!(a.ssid(), b.ssid());
    // A literal `+` in a password has to be sent as `%2B`; `+` decodes to a
    // space, which is what every form encoder means by it.
    let form = parse_wifi_form(b"ssid=Example-Wifi1&psk=a%2Bb").unwrap();
    assert_eq!(form.psk(), b"a+b");
}

#[test]
fn a_psk_containing_the_delimiters_survives() {
    // `&` and `=` are the delimiters; a password containing them is exactly
    // what a naive parser splits in the wrong place.
    let form = parse_wifi_form(b"ssid=Example-Wifi1&psk=a%26b%3Dc9").unwrap();
    assert_eq!(form.psk(), b"a&b=c9");
    assert_eq!(form.psk_len(), 6);
    // An *unencoded* `=` inside a value is legal and belongs to the value,
    // because only the first `=` separates key from value.
    let form = parse_wifi_form(b"ssid=Example-Wifi1&psk=a=b=c").unwrap();
    assert_eq!(form.psk(), b"a=b=c");
    // An unencoded `&`, however, ends the value - that is the format, not a
    // bug, and it is why a page must encode the password.
    let form = parse_wifi_form(b"ssid=Example-Wifi1&psk=a&b=c").unwrap();
    assert_eq!(form.psk(), b"a");
}

#[test]
fn a_truncated_escape_is_refused() {
    for body in [
        &b"ssid=abc%"[..],
        b"ssid=abc%4",
        b"ssid=abc%zz",
        b"ssid=abc%4z",
        b"ssid=Example-Wifi1&psk=%",
        b"ssid=Example-Wifi1&psk=%2",
    ] {
        assert_eq!(
            parse_wifi_form(body),
            Err(FormError::BadEscape),
            "{:?}",
            core::str::from_utf8(body)
        );
    }
}

#[test]
fn a_duplicate_key_is_refused_rather_than_resolved() {
    // "Last one wins" is the usual answer, and it is the wrong one here: it
    // would make `psk=right&psk=wrong` a coin toss about what goes to flash.
    for body in [
        &b"ssid=a&ssid=b&psk="[..],
        b"ssid=Example-Wifi1&psk=one&psk=two",
        b"ssid=Example-Wifi1&psk=&pin=1&pin=2",
        b"ssid=Example-Wifi1&counter=1&counter=2",
    ] {
        assert_eq!(parse_wifi_form(body), Err(FormError::DuplicateKey));
    }
}

#[test]
fn unknown_keys_are_ignored() {
    let form =
        parse_wifi_form(b"form=wifi&ssid=Example-Wifi1&psk=password9&submit=Join&csrf=abc123")
            .unwrap();
    assert_eq!(form.ssid(), SSID.as_bytes());
    assert_eq!(form.psk(), PSK.as_bytes());
    // Including one too long to be any key of ours, which must not be
    // mistaken for a malformed body.
    let long = "z".repeat(200);
    let form = parse_wifi_form(format!("{long}=1&ssid=Example-Wifi1").as_bytes()).unwrap();
    assert_eq!(form.ssid(), SSID.as_bytes());
}

#[test]
fn empty_pairs_and_trailing_ampersands_are_tolerated() {
    let form = parse_wifi_form(b"&&ssid=Example-Wifi1&&psk=password9&&").unwrap();
    assert_eq!(form.ssid(), SSID.as_bytes());
    assert_eq!(form.psk(), PSK.as_bytes());
    // A bare key with no `=` is a key with an empty value.
    let form = parse_wifi_form(b"ssid=Example-Wifi1&psk").unwrap();
    assert!(form.is_open());
}

#[test]
fn the_pin_and_counter_are_parsed_and_ignored() {
    let form = parse_wifi_form(b"ssid=Example-Wifi1&psk=&pin=2468&counter=17").unwrap();
    assert_eq!(form.pin.as_deref(), Some("2468"));
    assert_eq!(form.counter, Some(17));
    assert_eq!(screeny_device_api::request::check_auth(form.auth()), Ok(()));

    assert_eq!(
        parse_wifi_form(b"ssid=Example-Wifi1&counter=twelve"),
        Err(FormError::BadCounter)
    );
    assert_eq!(
        parse_wifi_form(b"ssid=Example-Wifi1&counter=99999999999"),
        Err(FormError::BadCounter)
    );
    let long_pin = "9".repeat(17);
    assert_eq!(
        parse_wifi_form(format!("ssid=Example-Wifi1&pin={long_pin}").as_bytes()),
        Err(FormError::BadPin)
    );
}

#[test]
fn a_body_longer_than_the_bound_is_refused_before_anything_else() {
    let body = vec![b'z'; MAX_FORM_LEN + 1];
    assert_eq!(parse_wifi_form(&body), Err(FormError::TooLong));
    // The longest legal body - every byte of every value escaped - still
    // fits under the bound.
    let ssid: String = (0..MAX_SSID_LEN).map(|_| "%61").collect();
    let psk: String = (0..MAX_PSK_LEN).map(|_| "%62").collect();
    let pin: String = (0..16).map(|_| "%39").collect();
    let body = format!("ssid={ssid}&psk={psk}&pin={pin}&counter=4294967295");
    assert!(
        body.len() <= MAX_FORM_LEN,
        "the worst legal body is {} bytes, MAX_FORM_LEN is {MAX_FORM_LEN}",
        body.len()
    );
    let form = parse_wifi_form(body.as_bytes()).unwrap();
    assert_eq!(form.ssid().len(), MAX_SSID_LEN);
    assert_eq!(form.psk_len(), MAX_PSK_LEN);
    assert_eq!(form.counter, Some(u32::MAX));
}

#[test]
fn every_form_error_has_an_http_answer() {
    for e in [
        FormError::TooLong,
        FormError::BadEscape,
        FormError::MissingSsid,
        FormError::SsidEmpty,
        FormError::SsidTooLong,
        FormError::PskTooLong,
        FormError::BadPin,
        FormError::BadCounter,
        FormError::DuplicateKey,
    ] {
        let reply = e.reply();
        assert_eq!(reply.error, e.code());
        assert!((400..500).contains(&reply.status()), "{e:?}");
        assert!(reply.detail.is_some(), "{e:?}");
        // The sentence never quotes the value, so it cannot quote a password.
        let text = reply.detail.unwrap();
        assert!(!text.contains("password9"), "{text}");
    }
    assert_eq!(FormError::TooLong.code(), ErrorCode::PayloadTooLarge);
}
