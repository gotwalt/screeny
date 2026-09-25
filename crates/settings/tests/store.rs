//! `screeny-settings` against `sequential-storage`'s RAM mock flash, on the
//! geometry of the real `screeny` partition: 16 pages of 4 KB, 4-byte words.
//!
//! These are the real map machinery, not a stub: every assertion below is
//! about bytes that went through `sequential-storage`'s item encoding, page
//! states and recycling.

use std::future::Future;
use std::task::{Context, Poll, Waker};

use screeny_settings::{
    key, Debounce, Field, Fields, IdleMode, LoadReport, Name, Psk, SchemaState, Scratch,
    SettingError, Settings, Ssid, Store, StoreError, Wifi, DEFAULT_BRIGHTNESS, SCHEMA_VERSION,
    SCRATCH_MIN,
};
use sequential_storage::cache::Cache;
use sequential_storage::map::{MapConfig, MapStorage};
use sequential_storage::mock_flash::{MockFlashBase, WriteCountCheck};

/// The `screeny` partition's geometry: 16 pages x 4 KB, 4-byte words.
type Flash = MockFlashBase<16, 4, 1024>;
const LEN: u32 = 16 * 4096;

/// The tests' Wi-Fi dummies. Never the real ones.
const SSID: &[u8] = b"Example-Wifi1";
const PSK: &[u8] = b"password9";

/// Run a `Store` future to completion.
///
/// The mock flash is synchronous, so nothing in the stack ever returns
/// `Pending`; a no-op waker and a bounded poll loop are enough, and they keep
/// the crate's dev-dependencies to the one it already needs. The bound turns
/// "this hung" into a failed test instead of a wedged run.
fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..1_000 {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
    }
    panic!("a store future did not finish: the mock flash never yields, so this is a bug");
}

fn blank_flash() -> Flash {
    // `alignment_check: true` on purpose: it panics on an unaligned write
    // buffer, which is exactly what `esp-storage` rejects on the device. It is
    // what keeps `Scratch`'s `#[repr(align(4))]` honest.
    Flash::new(WriteCountCheck::Twice, None, true)
}

fn store(flash: Flash) -> Store<Flash> {
    Store::new(flash, 0..LEN).expect("16 x 4 KB is a valid map range")
}

fn wifi() -> Wifi {
    Wifi::new(SSID, PSK).unwrap()
}

/// Write a raw item through the map, behind `Store`'s back, so a test can put
/// something in flash that `Store` would never write.
fn poke_bytes(flash: Flash, k: u8, v: &[u8]) -> Flash {
    let mut map = MapStorage::<u8, _, _>::new(
        flash,
        MapConfig::try_new(0..LEN).unwrap(),
        Cache::new_uncached(),
    );
    let mut scratch = Scratch::new();
    block_on(map.store_item::<&[u8]>(scratch.as_mut_slice(), &k, &v)).unwrap();
    map.destroy().0
}

fn poke_u8(flash: Flash, k: u8, v: u8) -> Flash {
    let mut map = MapStorage::<u8, _, _>::new(
        flash,
        MapConfig::try_new(0..LEN).unwrap(),
        Cache::new_uncached(),
    );
    let mut scratch = Scratch::new();
    block_on(map.store_item::<u8>(scratch.as_mut_slice(), &k, &v)).unwrap();
    map.destroy().0
}

fn load(st: &mut Store<Flash>) -> (Settings, LoadReport) {
    let mut scratch = Scratch::new();
    block_on(st.load(scratch.as_mut_slice()))
}

// -- blank flash ----------------------------------------------------------

#[test]
fn blank_flash_loads_defaults_and_is_not_an_error() {
    let mut st = store(blank_flash());
    let (settings, report) = load(&mut st);

    assert_eq!(settings, Settings::default());
    assert_eq!(settings.wifi, None);
    assert_eq!(settings.brightness, DEFAULT_BRIGHTNESS);
    assert_eq!(settings.idle_mode, IdleMode::Status);
    assert!(settings.name.is_empty(), "empty name means screeny-<id>");

    assert_eq!(report.schema, SchemaState::Blank);
    assert_eq!(report.error, None, "a blank partition is not a fault");
    assert!(report.is_blank());
    assert!(!report.is_clean());
    assert!(report.fallback.contains(Fields::ALL));
}

#[test]
fn a_blank_load_does_not_write_anything() {
    let mut st = store(blank_flash());
    let before = st.flash().stats_snapshot();
    let _ = load(&mut st);
    let d = before.compare_to(st.flash().stats_snapshot());
    assert_eq!(
        (d.writes, d.erases),
        (0, 0),
        "load must never write or erase"
    );
}

// -- round trip -----------------------------------------------------------

#[test]
fn every_key_round_trips() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    let buf = scratch.as_mut_slice();

    block_on(st.save_wifi(buf, &wifi())).unwrap();
    block_on(st.save_name(buf, &Name::new("kitchen").unwrap())).unwrap();
    block_on(st.save_brightness(buf, 137)).unwrap();
    block_on(st.save_idle_mode(buf, IdleMode::Dim)).unwrap();

    let (settings, report) = load(&mut st);
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.schema, SchemaState::Current);
    assert_eq!(settings.wifi, Some(wifi()));
    assert_eq!(settings.wifi.as_ref().unwrap().psk.as_bytes(), PSK);
    assert_eq!(settings.name.as_str(), "kitchen");
    assert_eq!(settings.brightness, 137);
    assert_eq!(settings.idle_mode, IdleMode::Dim);
}

#[test]
fn the_schema_version_is_written_on_the_first_save() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    block_on(st.save_brightness(scratch.as_mut_slice(), 12)).unwrap();

    let mut map = MapStorage::<u8, _, _>::new(
        st.flash().clone(),
        MapConfig::try_new(0..LEN).unwrap(),
        Cache::new_uncached(),
    );
    let stored =
        block_on(map.fetch_item::<u8>(scratch.as_mut_slice(), &key::SCHEMA_VERSION)).unwrap();
    assert_eq!(stored, Some(SCHEMA_VERSION));
}

#[test]
fn every_idle_mode_round_trips() {
    for mode in [
        IdleMode::Status,
        IdleMode::HoldForever,
        IdleMode::Dim,
        IdleMode::Black,
    ] {
        let mut scratch = Scratch::new();
        let mut st = store(blank_flash());
        block_on(st.save_idle_mode(scratch.as_mut_slice(), mode)).unwrap();
        assert_eq!(load(&mut st).0.idle_mode, mode);
    }
}

#[test]
fn an_open_network_round_trips_with_an_empty_psk() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    let open = Wifi::new(b"screeny-guest", b"").unwrap();
    block_on(st.save_wifi(scratch.as_mut_slice(), &open)).unwrap();

    let (settings, report) = load(&mut st);
    let got = settings.wifi.unwrap();
    assert_eq!(got.ssid.as_bytes(), b"screeny-guest");
    assert!(got.psk.is_empty(), "0 bytes = open network");
    assert!(!report.fallback.contains(Fields::WIFI));
}

#[test]
fn the_longest_values_round_trip_in_the_documented_scratch_buffer() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    let long = Wifi::new(&[b'S'; 32], &[b'P'; 64]).unwrap();
    let name = Name::new(&"n".repeat(32)).unwrap();

    block_on(st.save_wifi(scratch.as_mut_slice(), &long)).unwrap();
    block_on(st.save_name(scratch.as_mut_slice(), &name)).unwrap();

    let (settings, report) = load(&mut st);
    assert_eq!(report.error, None, "{report:?}");
    assert!(!report.fallback.contains(Fields::WIFI));
    assert!(!report.fallback.contains(Fields::NAME));
    assert_eq!(settings.wifi, Some(long));
    assert_eq!(settings.name.as_str().len(), 32);
}

/// A scratch buffer of a chosen size, aligned the way [`Scratch`] is.
#[repr(align(4))]
struct Aligned<const N: usize>([u8; N]);

impl<const N: usize> Aligned<N> {
    fn new() -> Self {
        Self([0; N])
    }
    fn buf(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

#[test]
fn the_documented_scratch_minimum_is_enough_and_below_it_is_a_clean_error() {
    // The floor really is a floor: the longest item round-trips in it.
    let mut aligned = Aligned::<SCRATCH_MIN>::new();
    let buf = aligned.buf();
    let mut st = store(blank_flash());
    let long = Wifi::new(&[b'S'; 32], &[b'P'; 64]).unwrap();
    block_on(st.save_wifi(buf, &long)).unwrap();
    assert_eq!(block_on(st.load(buf)).0.wifi, Some(long));

    // One byte under it, nothing is attempted.
    let mut small = [0u8; SCRATCH_MIN - 1];
    let before = st.flash().stats_snapshot();
    let (settings, report) = block_on(st.load(&mut small));
    assert_eq!(settings, Settings::default());
    assert_eq!(report.error, Some(StoreError::Scratch(SCRATCH_MIN)));
    assert_eq!(report.schema, SchemaState::Unreadable);
    assert_eq!(
        block_on(st.save_brightness(&mut small, 3)),
        Err(StoreError::Scratch(SCRATCH_MIN))
    );
    let d = before.compare_to(st.flash().stats_snapshot());
    assert_eq!((d.writes, d.erases, d.reads), (0, 0, 0));
}

// -- schema and unknown keys ----------------------------------------------

#[test]
fn an_unknown_schema_version_falls_back_to_defaults() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    block_on(st.save_brightness(scratch.as_mut_slice(), 200)).unwrap();
    block_on(st.save_name(scratch.as_mut_slice(), &Name::new("attic").unwrap())).unwrap();

    // A version from a build that does not exist yet.
    let flash = poke_u8(st.flash().clone(), key::SCHEMA_VERSION, 99);
    let mut st = store(flash);

    let (settings, report) = load(&mut st);
    assert_eq!(report.schema, SchemaState::Unknown(99));
    assert_eq!(report.error, None, "a future version is not a fault");
    assert_eq!(
        settings,
        Settings::default(),
        "ignore the rest, use defaults"
    );
}

#[test]
fn the_next_save_after_an_unknown_schema_version_rewrites_the_partition() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    block_on(st.save_brightness(scratch.as_mut_slice(), 200)).unwrap();
    let flash = poke_u8(st.flash().clone(), key::SCHEMA_VERSION, 99);

    let mut st = store(flash);
    block_on(st.save_idle_mode(scratch.as_mut_slice(), IdleMode::Black)).unwrap();

    let (settings, report) = load(&mut st);
    assert_eq!(report.schema, SchemaState::Current);
    assert_eq!(settings.idle_mode, IdleMode::Black);
    assert_eq!(
        settings.brightness, DEFAULT_BRIGHTNESS,
        "the foreign-version items are gone, not reinterpreted"
    );
}

#[test]
fn an_unknown_key_is_ignored_not_an_error() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    block_on(st.save_brightness(scratch.as_mut_slice(), 77)).unwrap();

    // Key 6 is the next one expected to be added (a boot counter);
    // key 200 is something further out still.
    let flash = poke_bytes(st.flash().clone(), 6, &[1, 2, 3, 4]);
    let flash = poke_bytes(flash, 200, b"a much longer future value");
    let mut st = store(flash);

    let (settings, report) = load(&mut st);
    assert_eq!(report.schema, SchemaState::Current);
    assert_eq!(report.error, None, "a key from the future is not a fault");
    assert!(!report.fallback.contains(Fields::BRIGHTNESS));
    assert_eq!(settings.brightness, 77);
}

#[test]
fn an_idle_mode_this_build_does_not_know_falls_back_without_an_error() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    block_on(st.save_brightness(scratch.as_mut_slice(), 77)).unwrap();
    let flash = poke_u8(st.flash().clone(), key::IDLE_MODE, 9);

    let mut st = store(flash);
    let (settings, report) = load(&mut st);
    assert_eq!(settings.idle_mode, IdleMode::Status);
    assert_eq!(
        settings.brightness, 77,
        "one bad field does not spoil the rest"
    );
    assert!(report.fallback.contains(Fields::IDLE_MODE));
    assert_eq!(report.error, None);
}

#[test]
fn a_stored_name_that_is_not_utf8_is_reported_and_defaulted() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    block_on(st.save_brightness(scratch.as_mut_slice(), 77)).unwrap();
    let flash = poke_bytes(st.flash().clone(), key::NAME, &[0xff, 0xfe, 0xfd]);

    let mut st = store(flash);
    let (settings, report) = load(&mut st);
    assert!(settings.name.is_empty());
    assert!(report.fallback.contains(Fields::NAME));
    assert_eq!(
        report.error,
        Some(StoreError::Invalid(SettingError::NameNotUtf8))
    );
    assert_eq!(settings.brightness, 77);
}

#[test]
fn a_stored_ssid_longer_than_the_schema_allows_is_reported_and_defaulted() {
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    block_on(st.save_brightness(scratch.as_mut_slice(), 77)).unwrap();
    let flash = poke_bytes(st.flash().clone(), key::WIFI_SSID, &[b'x'; 40]);

    let mut st = store(flash);
    let (settings, report) = load(&mut st);
    assert_eq!(settings.wifi, None);
    assert!(report.fallback.contains(Fields::WIFI));
    assert!(report.error.is_some());
}

// -- validation -----------------------------------------------------------

#[test]
fn oversize_values_are_refused_before_the_type_exists() {
    assert_eq!(Ssid::new(&[b'x'; 33]), Err(SettingError::SsidTooLong));
    assert_eq!(Ssid::new(b""), Err(SettingError::SsidEmpty));
    assert_eq!(Psk::new(&[b'x'; 65]), Err(SettingError::PskTooLong));
    assert_eq!(
        Name::new(&"x".repeat(33)),
        Err(SettingError::NameTooLong),
        "32 bytes is MAX_NAME_LEN"
    );
    assert_eq!(
        Name::from_bytes(&[0x80]),
        Err(SettingError::NameNotUtf8),
        "a name is UTF-8; an SSID is not required to be"
    );
    // At the limit, all fine.
    assert!(Ssid::new(&[b'x'; 32]).is_ok());
    assert!(Psk::new(&[b'x'; 64]).is_ok());
    assert!(Name::new(&"x".repeat(32)).is_ok());
}

#[test]
fn nothing_reaches_flash_when_a_value_is_refused() {
    // An oversize value cannot be built at all, so the only refusals `Store`
    // itself has to make are the scratch-buffer floor and the empty SSID.
    let mut scratch = Scratch::new();
    let mut st = store(blank_flash());
    let one_byte_ssid = Wifi {
        ssid: Ssid::new(b"x").unwrap(),
        psk: Psk::open(),
    };
    // A one-byte SSID is legal and does reach flash...
    let before = st.flash().stats_snapshot();
    assert!(block_on(st.save_wifi(scratch.as_mut_slice(), &one_byte_ssid)).is_ok());
    assert!(before.compare_to(st.flash().stats_snapshot()).writes > 0);

    // ...but a short buffer is refused with nothing read, written or erased.
    let before = st.flash().stats_snapshot();
    let mut too_small = [0u8; 8];
    assert_eq!(
        block_on(st.save_wifi(&mut too_small, &one_byte_ssid)),
        Err(StoreError::Scratch(SCRATCH_MIN))
    );
    assert_eq!(
        block_on(st.save_name(&mut too_small, &Name::new("x").unwrap())),
        Err(StoreError::Scratch(SCRATCH_MIN))
    );
    assert_eq!(
        block_on(st.save_idle_mode(&mut too_small, IdleMode::Dim)),
        Err(StoreError::Scratch(SCRATCH_MIN))
    );
    assert_eq!(
        block_on(st.clear_wifi(&mut too_small)),
        Err(StoreError::Scratch(SCRATCH_MIN))
    );
    let d = before.compare_to(st.flash().stats_snapshot());
    assert_eq!((d.writes, d.erases, d.reads), (0, 0, 0));
}

// -- write skipping -------------------------------------------------------

#[test]
fn a_save_of_the_stored_value_writes_nothing() {
    use screeny_settings::Write;
    let mut scratch = Scratch::new();
    let buf = scratch.as_mut_slice();
    let mut st = store(blank_flash());

    assert_eq!(block_on(st.save_brightness(buf, 40)), Ok(Write::Committed));
    assert_eq!(block_on(st.save_brightness(buf, 40)), Ok(Write::Skipped));

    let before = st.flash().stats_snapshot();
    assert_eq!(block_on(st.save_brightness(buf, 40)), Ok(Write::Skipped));
    let d = before.compare_to(st.flash().stats_snapshot());
    assert_eq!((d.writes, d.erases), (0, 0));

    let name = Name::new("hall").unwrap();
    assert_eq!(block_on(st.save_name(buf, &name)), Ok(Write::Committed));
    assert_eq!(block_on(st.save_name(buf, &name)), Ok(Write::Skipped));
    // An empty name on a store that has never had one is also a no-op.
    let mut fresh = store(blank_flash());
    let mut aligned = Aligned::<128>::new();
    let b2 = aligned.buf();
    block_on(fresh.save_brightness(b2, 1)).unwrap();
    assert_eq!(
        block_on(fresh.save_name(b2, &Name::empty())),
        Ok(Write::Skipped)
    );

    assert_eq!(
        block_on(st.save_idle_mode(buf, IdleMode::Status)),
        Ok(Write::Committed),
        "an explicit Status is still recorded, so a later default change cannot move it"
    );
    assert_eq!(
        block_on(st.save_idle_mode(buf, IdleMode::Status)),
        Ok(Write::Skipped)
    );

    assert_eq!(block_on(st.save_wifi(buf, &wifi())), Ok(Write::Committed));
    assert_eq!(block_on(st.save_wifi(buf, &wifi())), Ok(Write::Skipped));
}

// -- clearing and erasing --------------------------------------------------

#[test]
fn clear_wifi_leaves_the_other_settings_alone() {
    use screeny_settings::Write;
    let mut scratch = Scratch::new();
    let buf = scratch.as_mut_slice();
    let mut st = store(blank_flash());

    block_on(st.save_wifi(buf, &wifi())).unwrap();
    block_on(st.save_brightness(buf, 11)).unwrap();
    block_on(st.save_name(buf, &Name::new("shed").unwrap())).unwrap();

    assert_eq!(block_on(st.clear_wifi(buf)), Ok(Write::Committed));
    assert_eq!(
        block_on(st.clear_wifi(buf)),
        Ok(Write::Skipped),
        "clearing a cleared store writes nothing"
    );

    let (settings, report) = load(&mut st);
    assert_eq!(settings.wifi, None);
    assert_eq!(settings.brightness, 11);
    assert_eq!(settings.name.as_str(), "shed");
    assert_eq!(report.schema, SchemaState::Current);
    assert_eq!(report.error, None);

    // And credentials can be set again afterwards.
    block_on(st.save_wifi(buf, &wifi())).unwrap();
    assert_eq!(load(&mut st).0.wifi, Some(wifi()));
}

#[test]
fn erase_all_is_a_factory_reset() {
    let mut scratch = Scratch::new();
    let buf = scratch.as_mut_slice();
    let mut st = store(blank_flash());
    block_on(st.save_wifi(buf, &wifi())).unwrap();
    block_on(st.save_brightness(buf, 200)).unwrap();

    block_on(st.erase_all()).unwrap();

    let (settings, report) = load(&mut st);
    assert_eq!(settings, Settings::default());
    assert!(report.is_blank());
    // The PSK bytes are physically gone, which `clear_wifi` does not promise.
    assert!(
        !st.flash().as_bytes().windows(PSK.len()).any(|w| w == PSK),
        "erase_all really removes the key material"
    );
}

#[test]
fn clear_wifi_does_not_promise_to_remove_the_psk_bytes() {
    // Recorded as a fact, not a complaint: spec 8.4 says the PSK is not
    // treated as a secret, and an append-only flash store cannot unwrite.
    let mut scratch = Scratch::new();
    let buf = scratch.as_mut_slice();
    let mut st = store(blank_flash());
    block_on(st.save_wifi(buf, &wifi())).unwrap();
    block_on(st.clear_wifi(buf)).unwrap();

    assert_eq!(load(&mut st).0.wifi, None, "logically forgotten");
    assert!(
        st.flash().as_bytes().windows(PSK.len()).any(|w| w == PSK),
        "but still physically present until the page recycles - erase_all is the real reset"
    );
}

// -- endurance -------------------------------------------------------------

#[test]
fn thousands_of_brightness_writes_still_round_trip() {
    let mut scratch = Scratch::new();
    let buf = scratch.as_mut_slice();
    let mut st = store(blank_flash());

    // Enough to fill the 64 KB partition with ~16-byte items several times
    // over. Each value differs from the last, so none is skipped.
    const N: u32 = 6_000;
    let before = st.flash().stats_snapshot();
    for i in 0..N {
        let level = (i % 251) as u8;
        block_on(st.save_brightness(buf, level))
            .unwrap_or_else(|e| panic!("write {i} failed: {e:?}"));
    }
    let d = before.compare_to(st.flash().stats_snapshot());
    assert!(
        d.erases >= 1,
        "{N} writes erased nothing - the partition never filled, so page recycling was not exercised"
    );
    println!(
        "endurance: {N} writes -> {} flash writes, {} erases",
        d.writes, d.erases
    );

    let last = ((N - 1) % 251) as u8;
    let (settings, report) = load(&mut st);
    assert_eq!(report.error, None, "{report:?}");
    assert_eq!(report.schema, SchemaState::Current);
    assert_eq!(settings.brightness, last);

    // And the other settings survive being written into a recycling store.
    block_on(st.save_wifi(buf, &wifi())).unwrap();
    for i in 0..500u32 {
        block_on(st.save_brightness(buf, (i % 251) as u8)).unwrap();
    }
    let (settings, report) = load(&mut st);
    assert_eq!(report.error, None, "{report:?}");
    assert_eq!(
        settings.wifi,
        Some(wifi()),
        "migrated across page recycling"
    );
}

// -- power failure ---------------------------------------------------------

#[test]
fn a_write_cut_short_leaves_the_old_value_or_the_new_one() {
    // `bytes_until_shutoff` makes the mock fail after n byte-operations, which
    // is the closest thing to pulling the plug mid-write.
    let mut scratch = Scratch::new();
    let mut checked = 0u32;

    for cut in 1..220u32 {
        let mut st = store(blank_flash());
        let buf = scratch.as_mut_slice();
        block_on(st.save_brightness(buf, 10)).unwrap();
        block_on(st.save_name(buf, &Name::new("before").unwrap())).unwrap();

        st.flash().bytes_until_shutoff = Some(cut);
        let outcome = block_on(st.save_name(buf, &Name::new("after").unwrap()));
        st.flash().bytes_until_shutoff = None;

        // Reopen, exactly as a reboot would.
        let mut st = store(st.flash().clone());
        let (settings, _report) = load(&mut st);
        let name = settings.name.as_str();
        assert!(
            name == "before" || name == "after",
            "cut at {cut}: name was {name:?} after {outcome:?}"
        );
        assert_eq!(
            settings.brightness, 10,
            "cut at {cut}: an unrelated key must not be damaged"
        );
        if outcome.is_err() {
            checked += 1;
        }
    }
    assert!(checked > 0, "no cut actually interrupted a write");
}

#[test]
fn a_wifi_write_cut_short_never_leaves_one_networks_name_with_anothers_key() {
    let old = Wifi::new(b"Example-Wifi1", b"password9").unwrap();
    let new = Wifi::new(b"Example-Wifi2", b"password8").unwrap();
    let mut scratch = Scratch::new();

    for cut in 1..260u32 {
        let mut st = store(blank_flash());
        let buf = scratch.as_mut_slice();
        block_on(st.save_wifi(buf, &old)).unwrap();

        st.flash().bytes_until_shutoff = Some(cut);
        let _ = block_on(st.save_wifi(buf, &new));
        st.flash().bytes_until_shutoff = None;

        let mut st = store(st.flash().clone());
        let got = load(&mut st).0.wifi;
        assert!(
            got.is_none() || got.as_ref() == Some(&old) || got.as_ref() == Some(&new),
            "cut at {cut}: got {got:?}, which pairs one network's name with another's key"
        );
    }
}

// -- garbage ---------------------------------------------------------------

#[test]
fn load_never_panics_on_a_partition_full_of_random_bytes() {
    // A tiny xorshift so the failures are reproducible from the seed printed
    // in the assertion, without a dependency.
    let mut state: u64 = 0x5eed_1234_abcd_0001;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    for round in 0..120u32 {
        // `Disabled` because the bytes did not get there by writing, so the
        // mock's per-word write bookkeeping would be about the mock, not us.
        let mut flash = Flash::new(WriteCountCheck::Disabled, None, true);
        let bytes = flash.as_bytes_mut();
        for chunk in bytes.chunks_mut(8) {
            let r = next().to_le_bytes();
            chunk.copy_from_slice(&r[..chunk.len()]);
        }
        let mut st = store(flash);
        let (settings, report) = load(&mut st);

        // Whatever comes back must be a *valid* Settings, not garbage: the
        // types cannot represent an oversize SSID, an over-long name or a
        // non-UTF-8 one.
        if let Some(w) = &settings.wifi {
            assert!((1..=32).contains(&w.ssid.len()), "round {round}");
            assert!(w.psk.len() <= 64, "round {round}");
        }
        assert!(settings.name.as_bytes().len() <= 32, "round {round}");
        let _ = report;
    }
}

#[test]
fn a_single_flipped_page_still_loads() {
    let mut scratch = Scratch::new();
    let buf = scratch.as_mut_slice();
    let mut st = store(blank_flash());
    block_on(st.save_wifi(buf, &wifi())).unwrap();
    block_on(st.save_brightness(buf, 55)).unwrap();

    let mut flash = st.flash().clone();
    for b in &mut flash.as_bytes_mut()[0..4096] {
        *b ^= 0x5a;
    }
    let mut st = store(flash);
    let (settings, _report) = load(&mut st);
    // No assertion on the contents - the point is that this returns at all.
    assert!(settings.brightness == 55 || settings.brightness == DEFAULT_BRIGHTNESS);
}

// -- card 063's acceptance -------------------------------------------------

#[test]
fn a_sixty_second_brightness_sweep_costs_a_handful_of_writes() {
    let mut scratch = Scratch::new();
    let buf = scratch.as_mut_slice();
    let mut st = store(blank_flash());
    let mut debounce = Debounce::new();
    let mut live = DEFAULT_BRIGHTNESS;

    block_on(st.save_brightness(buf, live)).unwrap();
    let before = st.flash().stats_snapshot();
    let mut commits = 0u32;
    let mut changes = 0u32;

    // Card 063: 60 s at 30 changes a second, then the slider is let go.
    let mut now_ms = 0u64;
    for step in 0..(60 * 30) {
        live = (step % 200) as u8;
        debounce.note_change(Field::Brightness, now_ms);
        changes += 1;
        now_ms += 1000 / 30;
        while let Some(field) = debounce.due(now_ms) {
            assert_eq!(field, Field::Brightness);
            block_on(st.save_brightness(buf, live)).unwrap();
            commits += 1;
        }
    }
    // Three seconds of quiet after the finger comes off.
    now_ms += 3_000;
    while let Some(_field) = debounce.due(now_ms) {
        block_on(st.save_brightness(buf, live)).unwrap();
        commits += 1;
    }

    let d = before.compare_to(st.flash().stats_snapshot());
    assert_eq!(changes, 1_800);
    assert!(
        commits <= 5,
        "{changes} changes became {commits} commits, which is not a handful"
    );
    assert!(
        d.writes <= 20,
        "{changes} changes became {} flash writes",
        d.writes
    );
    assert_eq!(d.erases, 0, "a sweep must not erase a page");
    assert_eq!(load(&mut st).0.brightness, live, "the last value did land");

    // Printed so the card's Log can quote a number rather than a bound.
    println!(
        "sweep: {changes} changes -> {commits} commits -> {} flash writes, {} erases",
        d.writes, d.erases
    );
}
