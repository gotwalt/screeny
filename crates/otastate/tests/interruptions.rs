//! Card 241's safety claim, run rather than argued.
//!
//! The claim is that from "the last byte is staged" to "the update is
//! confirmed" there is **no instant at which losing power leaves a device that
//! does not boot**, and that at every one of those instants it boots something
//! known: either the image it was running before, or the new one on trial.
//! These tests walk every instant of the card's interruption table against the
//! paper bootloader in `screeny_otastate::model`, which is ESP-IDF v6.1's
//! selection code and `esp-bootloader-esp-idf 0.6.0`'s sequence arithmetic
//! written down.

use screeny_otastate::model::{
    every_cut, upload_and_activate, Cut, Damage, Device, Image, OtaData, Ran,
};
use screeny_otastate::{Boot, FwSlot, FwState, RevertReason};

/// A device as `tools/fw-run.sh` leaves it, booted once.
fn flashed() -> Device {
    let mut dev = Device::freshly_flashed();
    dev.power_on();
    assert_eq!(dev.run(), Ran::Settled);
    dev
}

#[test]
fn a_serial_flashed_device_runs_ota_0_and_has_nothing_to_prove() {
    let dev = flashed();
    assert_eq!(dev.running, Some(0));
    assert_eq!(dev.boot, Boot::Settled);
    // The bootloader wrote the entry itself, VALID, which is why the firmware
    // never runs the trial machinery on a device that was flashed over serial.
    assert_eq!(dev.otadata.current_state(), Ok(FwState::Valid));
    assert_eq!(dev.otadata.current_app(), Ok(Some(0)));
}

#[test]
fn a_settled_boot_writes_nothing_to_otadata_however_many_times_it_reboots() {
    let mut dev = flashed();
    let before = dev.otadata;
    for _ in 0..5 {
        dev.power_on();
        assert_eq!(dev.run(), Ran::Settled);
    }
    assert_eq!(
        dev.otadata, before,
        "a boot that is not a trial must not rewrite otadata"
    );
}

#[test]
fn a_good_update_boots_on_trial_and_then_confirms_itself() {
    let mut dev = flashed();
    assert_eq!(dev.inactive(), Some(1));
    assert!(upload_and_activate(&mut dev, Image::Good, Cut::None));

    dev.power_on();
    assert_eq!(dev.running, Some(1), "the staged slot boots");
    assert_eq!(dev.boot, Boot::Trial, "and it boots on trial");
    assert_eq!(dev.otadata.current_state(), Ok(FwState::PendingVerify));

    assert_eq!(dev.run(), Ran::Confirmed);
    assert_eq!(dev.otadata.current_state(), Ok(FwState::Valid));

    // And it survives a power cycle, which is the whole point of otadata.
    dev.power_on();
    assert_eq!(dev.running, Some(1));
    assert_eq!(dev.boot, Boot::Settled);
}

// ---------------------------------------------------------------------------
// The interruption table
// ---------------------------------------------------------------------------

#[test]
fn every_interruption_of_an_activation_leaves_a_device_that_boots() {
    for cut in every_cut() {
        let mut dev = flashed();
        let reached_reset = upload_and_activate(&mut dev, Image::Good, cut);
        // Whether or not the activation reached its own reset, the next event
        // is a boot: losing power *is* a reset.
        dev.power_on();
        assert!(
            dev.running.is_some(),
            "{cut:?}: the device did not boot at all"
        );
        let ran = dev.slots[dev.running.unwrap() as usize];
        assert!(
            ran == Image::Good,
            "{cut:?}: booted {ran:?}, which is not a working image"
        );
        assert!(reached_reset || cut != Cut::None);
    }
}

#[test]
fn an_interrupted_sequence_write_boots_the_image_that_was_already_running() {
    // Row 5a. The entry being written is never the one the device is booting
    // from, so the untouched entry wins and nothing has changed.
    for damage in [Damage::Erased, Damage::Torn] {
        let mut dev = flashed();
        upload_and_activate(&mut dev, Image::Good, Cut::DuringSelect(damage));
        dev.power_on();
        assert_eq!(dev.running, Some(0), "{damage:?}");
    }
}

#[test]
fn an_activation_interrupted_before_the_state_write_still_puts_the_image_on_trial() {
    // Row 5b, and the one window that would otherwise make an unproven image
    // permanent: the sequence says "boot ota_1" and the state says nothing, so
    // the bootloader hands over without marking anything.
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::Good, Cut::BeforeMarkNew);
    dev.power_on();
    assert_eq!(dev.running, Some(1), "the staged slot is selected");
    assert_eq!(
        dev.boot,
        Boot::Unproven,
        "and the bootloader did not put it on trial"
    );
    assert!(dev.boot.on_trial(), "so the firmware does");
    assert_eq!(dev.run(), Ran::Confirmed);
    assert_eq!(dev.otadata.current_state(), Ok(FwState::Valid));
}

#[test]
fn an_unproven_image_that_dies_is_still_rolled_back() {
    // The promotion of row 5b is worth something only if it arms the
    // bootloader for the *next* reset. This is that.
    let mut dev = Device::freshly_flashed();
    dev.power_on();
    dev.run();
    upload_and_activate(&mut dev, Image::Panics, Cut::BeforeMarkNew);
    dev.power_on();
    assert_eq!(dev.running, Some(1));
    assert_eq!(dev.boot, Boot::Unproven);
    assert_eq!(dev.run(), Ran::ResetWithoutConfirming);

    dev.power_on();
    assert_eq!(dev.running, Some(0), "back on the image that worked");
    assert_eq!(dev.boot, Boot::Reverted(RevertReason::Aborted));
}

#[test]
fn an_interrupted_state_write_boots_the_image_that_was_already_running() {
    for damage in [Damage::Erased, Damage::Torn] {
        let mut dev = flashed();
        upload_and_activate(&mut dev, Image::Good, Cut::DuringMarkNew(damage));
        dev.power_on();
        assert_eq!(dev.running, Some(0), "{damage:?}");
    }
}

#[test]
fn losing_power_between_the_last_write_and_the_reset_is_simply_the_update() {
    // Row 6: committed. The reset the firmware was about to do is the same
    // reset the power cut caused.
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::Good, Cut::BeforeReset);
    dev.power_on();
    assert_eq!(dev.running, Some(1));
    assert_eq!(dev.boot, Boot::Trial);
}

// ---------------------------------------------------------------------------
// The three ways a trial ends badly
// ---------------------------------------------------------------------------

#[test]
fn an_image_that_never_becomes_healthy_is_reverted_at_the_deadline() {
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::NeverHealthy, Cut::None);
    dev.power_on();
    assert_eq!(dev.running, Some(1));
    assert_eq!(dev.run(), Ran::RevertedAtDeadline);

    dev.power_on();
    assert_eq!(dev.running, Some(0), "the previous image is back");
    assert_eq!(dev.boot, Boot::Reverted(RevertReason::Deadline));
    assert_eq!(dev.run(), Ran::Settled, "and it stays back");
}

#[test]
fn a_panic_during_the_trial_is_rolled_back_by_the_bootloader_alone() {
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::Panics, Cut::None);
    dev.power_on();
    assert_eq!(dev.running, Some(1));
    assert_eq!(
        dev.run(),
        Ran::ResetWithoutConfirming,
        "the app writes nothing at all on this path"
    );

    dev.power_on();
    assert_eq!(dev.running, Some(0));
    assert_eq!(dev.boot, Boot::Reverted(RevertReason::Aborted));
}

#[test]
fn a_reset_that_is_not_a_panic_is_rolled_back_the_same_way() {
    // ESP-IDF's abort loop does not look at the reset reason, so a watchdog, a
    // brownout, the EN button and somebody pulling the cable are the same
    // event. `Image::Hangs` is the watchdog case: nothing the app does, just a
    // reset.
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::Hangs, Cut::None);
    dev.power_on();
    assert_eq!(dev.run(), Ran::ResetWithoutConfirming);
    dev.power_on();
    assert_eq!(dev.running, Some(0));
    assert_eq!(dev.boot, Boot::Reverted(RevertReason::Aborted));
}

#[test]
fn a_crash_loop_cannot_happen_because_the_second_boot_is_the_old_image() {
    // Card 243's crash-loop guard stops at five quick panics. An OTA that
    // panics must never get near it: one panic is all it takes.
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::Panics, Cut::None);
    assert_eq!(dev.settle(5), 2, "one trial boot, then the old image");
}

#[test]
fn power_lost_during_the_confirm_write_un_installs_rather_than_half_installs() {
    // Row 9. The confirm writes the *active* entry, so an interrupted one
    // leaves the other entry as the only valid one - the previous image.
    for damage in [Damage::Erased, Damage::Torn] {
        let mut dev = flashed();
        upload_and_activate(&mut dev, Image::Good, Cut::None);
        dev.power_on();
        assert_eq!(dev.boot, Boot::Trial);
        let _ = dev.otadata.set_state(FwState::Valid, Some(damage));

        dev.power_on();
        assert_eq!(
            dev.running,
            Some(0),
            "{damage:?}: an interrupted confirm reverts, which is the safe direction"
        );
    }
}

#[test]
fn power_lost_during_the_revert_write_still_boots_the_previous_image() {
    // Row 10. The write was trying to arrange exactly this, so losing it
    // changes nothing: the entry is unusable either way.
    for damage in [Damage::Erased, Damage::Torn] {
        let mut dev = flashed();
        upload_and_activate(&mut dev, Image::NeverHealthy, Cut::None);
        dev.power_on();
        assert_eq!(dev.boot, Boot::Trial);
        let _ = dev.otadata.set_state(FwState::Invalid, Some(damage));

        dev.power_on();
        assert_eq!(dev.running, Some(0), "{damage:?}");
    }
}

// ---------------------------------------------------------------------------
// The trap: what happens to the *next* update after a rollback
// ---------------------------------------------------------------------------

#[test]
fn an_update_after_a_rollback_still_activates() {
    // `Ota::current_app_partition` ignores the states, so after a rollback it
    // names the slot that was rolled back from - which is the slot the next
    // upload stages into. `set_current_app_partition` then writes no sequence
    // at all ("no need to update any sequence if the partition isn't
    // changed"), and the whole activation rests on `set_current_ota_state(New)`
    // clearing ABORTED on an entry whose sequence is already the highest.
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::Panics, Cut::None);
    assert_eq!(dev.settle(5), 2);
    assert_eq!(dev.running, Some(0));
    assert_eq!(dev.boot, Boot::Reverted(RevertReason::Aborted));
    assert_eq!(
        dev.otadata.current_app(),
        Ok(Some(1)),
        "otadata still names the slot it rolled back from"
    );

    // Now a good image into the same slot.
    assert_eq!(dev.inactive(), Some(1));
    upload_and_activate(&mut dev, Image::Good, Cut::None);
    dev.power_on();
    assert_eq!(dev.running, Some(1), "the retry really does boot");
    assert_eq!(dev.boot, Boot::Trial);
    assert_eq!(dev.run(), Ran::Confirmed);
}

#[test]
fn updates_alternate_slots_for_as_long_as_they_keep_working() {
    let mut dev = flashed();
    let mut expect = 0u8;
    for round in 0..6 {
        expect = 1 - expect;
        assert!(upload_and_activate(&mut dev, Image::Good, Cut::None));
        dev.power_on();
        assert_eq!(dev.running, Some(expect), "round {round}");
        assert_eq!(dev.boot, Boot::Trial, "round {round}");
        assert_eq!(dev.run(), Ran::Confirmed, "round {round}");
    }
}

#[test]
fn a_run_of_bad_updates_never_leaves_the_device_off_the_air() {
    // Six updates in a row, each one bad in a different way, each one reverted.
    // The device runs the same good image throughout.
    let mut dev = flashed();
    for bad in [
        Image::NeverHealthy,
        Image::Panics,
        Image::Hangs,
        Image::NeverHealthy,
        Image::Panics,
        Image::Hangs,
    ] {
        upload_and_activate(&mut dev, bad, Cut::None);
        assert_eq!(dev.settle(5), 2, "{bad:?} took more than one retry");
        assert_eq!(dev.running, Some(0), "{bad:?}");
        assert!(dev.boot.revert_reason().is_some(), "{bad:?}");
    }
}

#[test]
fn during_a_trial_the_inactive_slot_is_the_image_we_may_have_to_go_back_to() {
    // Which is why the firmware answers `busy` to an upload while a trial is
    // running: staging over that slot would throw the escape hatch away.
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::Good, Cut::None);
    dev.power_on();
    assert_eq!(dev.boot, Boot::Trial);
    assert_eq!(dev.inactive(), Some(0));
    assert_eq!(
        dev.slots[dev.inactive().unwrap() as usize],
        Image::Good,
        "the known-good image is sitting in the slot an upload would erase"
    );
}

// ---------------------------------------------------------------------------
// The bootloader's own floor
// ---------------------------------------------------------------------------

#[test]
fn a_structurally_broken_image_never_runs_even_if_otadata_selects_it() {
    // The bootloader verifies before it hands over, so this is the second net
    // under card 240's validator rather than the first.
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::Broken, Cut::None);
    dev.power_on();
    assert_eq!(dev.running, Some(0), "it fell back to the image that works");
    assert_eq!(dev.boot, Boot::Reverted(RevertReason::Rejected));
}

#[test]
fn an_otadata_with_nothing_usable_in_it_boots_ota_0() {
    // "No factory image, trying OTA 0" - the floor under every interrupted
    // write, and the reason none of them can leave a device that boots nothing.
    let mut dev = Device::freshly_flashed();
    dev.otadata = OtaData::erased();
    dev.power_on();
    assert_eq!(dev.running, Some(0));
}

#[test]
fn a_device_that_cannot_read_otadata_still_runs_and_says_it_does_not_know() {
    // One torn entry makes both of the crate's reads fail, so the firmware
    // reports `unknown` and writes nothing. It is the one state from which an
    // OTA cannot proceed, and `tools/fw-run.sh` is what clears it.
    let mut dev = flashed();
    upload_and_activate(&mut dev, Image::Good, Cut::DuringSelect(Damage::Torn));
    dev.power_on();
    assert_eq!(dev.running, Some(0), "it still boots");
    assert_eq!(dev.boot, Boot::Unknown);
    assert_eq!(dev.run(), Ran::Settled, "and writes nothing");
}

#[test]
fn the_slot_words_are_the_ones_the_api_reports() {
    // A cheap guard against the two enums drifting: `FwSlot` is what
    // `GET /api/v1/status` carries and what this crate reasons in.
    let mut dev = flashed();
    assert_eq!(dev.running, Some(0));
    upload_and_activate(&mut dev, Image::Good, Cut::None);
    dev.power_on();
    assert_eq!(dev.running, Some(1));
    assert_eq!(
        screeny_otastate::classify(FwSlot::Ota1, FwSlot::Ota1, FwState::PendingVerify),
        dev.boot
    );
}
