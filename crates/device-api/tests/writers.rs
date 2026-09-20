//! Where the three JSON writers differ, written down as a test rather than as
//! a memory.
//!
//! The firmware writes replies with **picoserve's** serialiser
//! (`picoserve::response::json`), reads requests with **serde-json-core**, and
//! the Studio uses **serde_json**. Two of the three are exercised directly
//! here; picoserve's is not a dependency of this crate (its serialiser is
//! private, reachable only through an async `picoserve::io::Write`), so its
//! two rules are asserted against a transcription of
//! `picoserve-0.20.0/src/response/json.rs:69-110`, which is short enough to
//! check by eye.
//!
//! The consequence, and the only one that matters: **all three parse each
//! other's output**, but a byte-for-byte comparison of the same value from two
//! of them can differ. Nothing in the product compares JSON bytes; the golden
//! files do, which is why they are kept clear of the two characters below.

mod common;

use common::wire_len;
use screeny_device_api::reply::SettingsReply;
use screeny_device_api::request::MIN_UNESCAPE_BUFFER;
use screeny_device_api::text::text;
use screeny_device_api::IdleMode;

fn named(name: &str) -> SettingsReply {
    SettingsReply {
        name: text(name).unwrap(),
        brightness: 96,
        idle_mode: IdleMode::Status,
    }
}

/// picoserve's escaper, transcribed from `src/response/json.rs`. The only
/// thing this crate needs from it is the set of characters it treats
/// specially.
fn picoserve_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\x08' => out.push_str("\\b"),
            '\x09' => out.push_str("\\t"),
            '\x0A' => out.push_str("\\n"),
            '\x0C' => out.push_str("\\f"),
            '\x0D' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '/' => out.push_str("\\/"),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[test]
fn the_two_ordinary_escapes_agree_everywhere() {
    // Quote, backslash, and the five short control escapes are the same in
    // all three, so any name a person types is byte-identical whichever
    // writer produced it.
    for name in ["a\"b", "a\\b", "a\nb", "a\tb", "line\r\nbreak"] {
        let value = named(name);
        let json = serde_json::to_string(&value).unwrap();
        let mut buf = [0u8; 512];
        let n = serde_json_core::to_slice(&value, &mut buf).unwrap();
        assert_eq!(json.as_bytes(), &buf[..n], "{name:?}");
        let (back, _) = serde_json_core::from_slice_escaped::<SettingsReply>(
            &buf[..n],
            &mut [0u8; MIN_UNESCAPE_BUFFER],
        )
        .unwrap();
        assert_eq!(back, value, "{name:?}");
        assert!(json.contains(&picoserve_escape(name)), "{json}");
    }
}

#[test]
fn a_slash_is_the_one_character_picoserve_writes_differently() {
    let value = named("up/down");
    let json = serde_json::to_string(&value).unwrap();
    assert!(json.contains("up/down"));
    assert_eq!(picoserve_escape("up/down"), "up\\/down");
    // Both spellings mean the same string to every parser, and this proves
    // the one the firmware would send still reads back.
    let from_picoserve = json.replace("up/down", "up\\/down");
    let back: SettingsReply = serde_json::from_str(&from_picoserve).unwrap();
    assert_eq!(back, value);
    let (back, _) = serde_json_core::from_slice_escaped::<SettingsReply>(
        from_picoserve.as_bytes(),
        &mut [0u8; MIN_UNESCAPE_BUFFER],
    )
    .unwrap();
    assert_eq!(back, value);
    // It is one byte longer, which the size bounds already allow for: they
    // budget six bytes per byte of text.
    assert_eq!(
        picoserve_escape("up/down").len(),
        "up/down".len() + 1
    );
}

#[test]
fn serde_json_core_spells_unicode_escapes_in_upper_case() {
    let value = named("a\u{1f}b");
    let json = serde_json::to_string(&value).unwrap();
    let mut buf = [0u8; 512];
    let n = serde_json_core::to_slice(&value, &mut buf).unwrap();
    let core = core::str::from_utf8(&buf[..n]).unwrap();

    assert!(json.contains("\\u001f"), "{json}");
    assert!(core.contains("\\u001F"), "{core}");
    assert_eq!(picoserve_escape("a\u{1f}b"), "a\\u001fb");
    // Same length, so the size bounds are unaffected...
    assert_eq!(json.len(), core.len());
    assert_eq!(wire_len(&value), json.len());
    // ...and both read back to the same value.
    let (back, _) = serde_json_core::from_slice_escaped::<SettingsReply>(
        json.as_bytes(),
        &mut [0u8; MIN_UNESCAPE_BUFFER],
    )
    .unwrap();
    assert_eq!(back, value);
    let back: SettingsReply = serde_json::from_str(core).unwrap();
    assert_eq!(back, value);
}

#[test]
fn a_multibyte_character_passes_through_all_three_unescaped() {
    let value = named("Caf\u{e9}");
    let json = serde_json::to_string(&value).unwrap();
    assert!(json.contains("Caf\u{e9}"), "{json}");
    assert_eq!(picoserve_escape("Caf\u{e9}"), "Caf\u{e9}");
    assert_eq!(wire_len(&value), json.len());
}

#[test]
fn serde_json_core_does_not_unescape_without_a_buffer() {
    // The footgun card 222 has to avoid. `from_slice` hands the visitor the
    // raw escaped text and reports no error at all, so a name posted with a
    // six-character `\u00e9` in it would be stored with those six characters.
    // Spelled in two pieces so that this source file itself contains no
    // escape sequence for an editor to normalise.
    const ESCAPED: &str = concat!("caf", "\\", "u00e9");
    let body = format!(r#"{{"name":"{ESCAPED}","brightness":96,"idle_mode":"status"}}"#);
    let body = body.as_bytes();

    let (wrong, _) = serde_json_core::from_slice::<SettingsReply>(body).unwrap();
    assert_eq!(wrong.name.as_str(), ESCAPED);
    assert_eq!(wrong.name.len(), 9);

    let (right, _) =
        serde_json_core::from_slice_escaped::<SettingsReply>(body, &mut [0u8; MIN_UNESCAPE_BUFFER])
            .unwrap();
    assert_eq!(right.name.as_str(), "caf\u{e9}");
    assert_eq!(right.name.len(), 5);

    // serde_json, which the Studio uses, always unescapes.
    let studio: SettingsReply = serde_json::from_slice(body).unwrap();
    assert_eq!(studio, right);
}

#[test]
fn the_unescape_buffer_is_as_long_as_the_longest_string_a_request_can_hold() {
    // 32 bytes: a name. picoserve's default `Json` extractor happens to use
    // exactly that, with no margin, which is worth knowing rather than
    // discovering.
    assert_eq!(MIN_UNESCAPE_BUFFER, 32);

    let name = "\"".repeat(32);            // 32 bytes unescaped, 64 escaped
    let value = named(&name);
    let json = serde_json::to_string(&value).unwrap();
    let (back, _) =
        serde_json_core::from_slice_escaped::<SettingsReply>(json.as_bytes(), &mut [0u8; 32])
            .unwrap();
    assert_eq!(back, value);

    // One byte less and it fails loudly, which is the good case.
    assert!(serde_json_core::from_slice_escaped::<SettingsReply>(
        json.as_bytes(),
        &mut [0u8; 31]
    )
    .is_err());
}
