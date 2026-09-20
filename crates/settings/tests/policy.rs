//! The parts with no flash in them: the value types and the write debounce.

use screeny_settings::{
    Debounce, Field, IdleMode, Name, Psk, SettingError, Settings, Ssid, Wifi, DEFAULT_BRIGHTNESS,
    QUIET_MS,
};

/// The dummies CLAUDE.md requires. Never a real credential, anywhere.
const SSID: &[u8] = b"Example-Wifi1";
const PSK: &[u8] = b"password9";

// -- the PSK never gets formatted (spec 8.4) ------------------------------

#[test]
fn debug_of_a_psk_shows_only_its_length() {
    let psk = Psk::new(PSK).unwrap();
    assert_eq!(format!("{psk:?}"), "Psk(<9 bytes>)");
    assert_eq!(format!("{:?}", Psk::open()), "Psk(<0 bytes>)");
}

#[test]
fn debug_of_a_wifi_does_not_contain_the_psk() {
    let w = Wifi::new(SSID, PSK).unwrap();
    let s = format!("{w:?}");
    assert!(!s.contains("password9"), "the PSK leaked through Debug: {s}");
    // Nor as a byte list: "password9" starts 112, 97, 115.
    assert!(!s.contains("112, 97, 115"), "the PSK leaked as bytes: {s}");
    assert!(s.contains("Example-Wifi1"), "the SSID is not a secret: {s}");
    assert!(s.contains("Psk(<9 bytes>)"), "{s}");
}

#[test]
fn debug_of_a_non_utf8_ssid_falls_back_to_bytes() {
    let w = Wifi::new(&[0x4e, 0xdf], PSK).unwrap();
    assert_eq!(format!("{w:?}"), "Wifi { ssid: Ssid([78, 223]), psk: Psk(<9 bytes>) }");
}

#[test]
fn debug_of_the_whole_settings_does_not_contain_the_psk() {
    let s = Settings {
        wifi: Some(Wifi::new(SSID, PSK).unwrap()),
        name: Name::new("kitchen").unwrap(),
        brightness: 96,
        idle_mode: IdleMode::Dim,
    };
    let text = format!("{s:?}");
    assert!(!text.contains("password9"), "{text}");
    assert!(text.contains("kitchen"));
}

#[test]
fn a_psk_with_no_debug_still_hands_its_bytes_to_the_radio() {
    // The one documented way to the bytes, so a reviewer can grep for it.
    assert_eq!(Psk::new(PSK).unwrap().as_bytes(), PSK);
}

// -- defaults --------------------------------------------------------------

#[test]
fn the_defaults_are_the_firmwares() {
    let d = Settings::default();
    assert_eq!(d.brightness, DEFAULT_BRIGHTNESS);
    assert_eq!(DEFAULT_BRIGHTNESS, 96, "firmware/src/display.rs:116");
    assert_eq!(d.idle_mode, IdleMode::Status);
    assert_eq!(d.wifi, None);
    assert!(d.name.is_empty(), "empty means screeny-<id>");
}

// -- validation ------------------------------------------------------------

#[test]
fn an_ssid_is_bytes_and_a_name_is_text() {
    // Plenty of real access points are not UTF-8; refusing them would be a bug.
    let latin1 = &[0x4e, 0x65, 0x74, 0xdf];
    let ssid = Ssid::new(latin1).expect("an SSID is an opaque byte string");
    assert_eq!(ssid.as_bytes(), latin1);
    assert_eq!(ssid.as_str(), None);

    // A name goes into a `heapless::String` and an mDNS instance name, so it
    // must be text.
    assert_eq!(Name::from_bytes(latin1), Err(SettingError::NameNotUtf8));
    assert_eq!(Name::from_bytes("Küche".as_bytes()).unwrap().as_str(), "Küche");
}

#[test]
fn the_limits_are_the_protocols() {
    assert_eq!(Ssid::new(&[b'a'; 32]).unwrap().len(), 32);
    assert_eq!(Ssid::new(&[b'a'; 33]), Err(SettingError::SsidTooLong));
    assert_eq!(Psk::new(&[b'a'; 64]).unwrap().len(), 64);
    assert_eq!(Psk::new(&[b'a'; 65]), Err(SettingError::PskTooLong));
    // Bytes, not characters: four 3-byte characters fit, eleven do not.
    assert!(Name::new(&"あ".repeat(10)).is_ok());
    assert_eq!(Name::new(&"あ".repeat(11)), Err(SettingError::NameTooLong));
}

// -- debounce --------------------------------------------------------------

#[test]
fn nothing_is_due_before_three_seconds_of_quiet() {
    let mut d = Debounce::new();
    assert!(d.is_idle());
    assert_eq!(d.due(0), None);

    d.note_change(Field::Brightness, 1_000);
    assert!(!d.is_idle());
    assert_eq!(d.due(1_000), None);
    assert_eq!(d.due(1_000 + QUIET_MS - 1), None);
    assert_eq!(d.due(1_000 + QUIET_MS), Some(Field::Brightness));
    assert_eq!(d.due(9_999_999), None, "taking it clears it");
    assert!(d.is_idle());
}

#[test]
fn repeated_changes_to_one_field_coalesce_into_one_write() {
    let mut d = Debounce::new();
    for t in 0..200u64 {
        d.note_change(Field::Brightness, t * 16); // ~60 Hz
    }
    // Still moving at the end of the drag: nothing is due.
    assert_eq!(d.due(200 * 16), None);
    // Quiet: exactly one commit for two hundred changes.
    assert_eq!(d.due(200 * 16 + QUIET_MS), Some(Field::Brightness));
    assert_eq!(d.due(200 * 16 + QUIET_MS), None);
}

#[test]
fn the_fields_are_independent() {
    let mut d = Debounce::new();
    d.note_change(Field::Brightness, 0);
    d.note_change(Field::Name, 2_000);

    assert_eq!(d.due(QUIET_MS), Some(Field::Brightness));
    assert_eq!(d.due(QUIET_MS), None, "the name is not quiet yet");
    assert_eq!(d.due(2_000 + QUIET_MS), Some(Field::Name));
    assert!(d.is_idle());
}

#[test]
fn a_change_can_be_cancelled_without_a_write() {
    let mut d = Debounce::new();
    d.note_change(Field::IdleMode, 0);
    d.cancel(Field::IdleMode);
    assert!(d.is_idle());
    assert_eq!(d.due(QUIET_MS * 10), None);
}

#[test]
fn next_due_in_ms_tells_the_task_how_long_to_sleep() {
    let mut d = Debounce::new();
    assert_eq!(d.next_due_in_ms(0), None, "nothing pending: wait for a change");

    d.note_change(Field::Brightness, 1_000);
    assert_eq!(d.next_due_in_ms(1_000), Some(QUIET_MS));
    assert_eq!(d.next_due_in_ms(2_000), Some(QUIET_MS - 1_000));
    assert_eq!(d.next_due_in_ms(10_000), Some(0), "already due");

    d.note_change(Field::Name, 9_000);
    assert_eq!(
        d.next_due_in_ms(10_000),
        Some(0),
        "the soonest of the pending fields"
    );
}

#[test]
fn a_late_poll_does_not_lose_a_pending_write() {
    // The store task may be starved for a long time by the frame path; when it
    // finally runs, the change is still there.
    let mut d = Debounce::new();
    d.note_change(Field::Brightness, 5);
    assert_eq!(d.due(5 + 60 * 60 * 1000), Some(Field::Brightness));
}

#[test]
fn there_is_no_wifi_field_to_debounce() {
    // Not an assertion so much as a note that survives a refactor: if someone
    // adds `Field::Wifi`, this stops compiling and they have to read
    // `Store::save_wifi`'s doc comment about writing before disconnecting.
    assert_eq!(Field::ALL.len(), 3);
    assert_eq!(
        Field::ALL,
        [Field::Name, Field::Brightness, Field::IdleMode]
    );
}

#[test]
fn a_sixty_second_sweep_in_bursts_is_still_a_handful() {
    // Six separate drags of ten seconds each, a second apart: one write per
    // drag, not 1800.
    let mut d = Debounce::new();
    let mut commits = 0;
    let mut now = 0u64;
    for _drag in 0..6 {
        for _ in 0..300 {
            d.note_change(Field::Brightness, now);
            now += 33;
            while d.due(now).is_some() {
                commits += 1;
            }
        }
        now += 4_000; // finger off
        while d.due(now).is_some() {
            commits += 1;
        }
    }
    assert_eq!(commits, 6, "one write per drag");
}
