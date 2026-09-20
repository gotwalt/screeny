//! What kind of boot this is, and when an image on trial has earned its keep.
//!
//! Card 240 got a new image safely into the inactive slot. Card 241 is the rest
//! of research 006's plan: **activate** it, boot it **on trial**, let it
//! **confirm** itself if it works, and put the old one back if it does not.
//! Three of those four are flash writes and a reboot, and they belong in the
//! firmware. What lives here is the part that is pure reasoning and therefore
//! the part a plain `cargo test` can hold to account:
//!
//! * [`classify`] - given what the MMU says is running, what `otadata` selected
//!   and what state that entry is in, **what kind of boot is this**: settled, on
//!   trial, unproven, or the aftermath of a revert.
//! * [`decide`] - given the uptime and three facts about the device, **has the
//!   image on trial proved itself**, should it be given longer, or is it time to
//!   put the old one back. Research 006 section 6's criterion, in one place.
//! * [`model`] - a paper `otadata` and a paper ESP-IDF v6.1 bootloader, so that
//!   "what boots if the power goes *here*" is answered by running it rather than
//!   by arguing about it. Not compiled into the firmware.
//!
//! ## Why the states are the wire's
//!
//! [`FwSlot`] and [`FwState`] come from `screeny-device-api`, which is already
//! what `GET /api/v1/status` reports and already spells the ESP-IDF image states
//! (`esp_ota_img_states_t`). A private copy here would be a third spelling of
//! the same six words, and the firmware would spend its time translating between
//! them on a path where a wrong translation reverts a good update.

#![no_std]
#![forbid(unsafe_code)]

pub use screeny_device_api::{FwSlot, FwState, RevertReason, UpdateOutcome};

#[cfg(feature = "model")]
pub mod model;

// ---------------------------------------------------------------------------
// The health criterion (research 006 section 6)
// ---------------------------------------------------------------------------

/// No image confirms itself before this, however healthy it looks.
///
/// Research 006 section 6: "no sooner than 60 s after boot, so that a
/// crash-after-30-seconds is still caught". It is deliberately the same number
/// as the firmware's crash-loop window (`panic::QUICK_MS`): "too early to count
/// as healthy" and "that was a quick death" must not disagree about what an
/// early death is.
pub const CONFIRM_NOT_BEFORE_MS: u32 = 60_000;

/// After this long, an image that nobody has talked to is allowed to confirm
/// anyway.
///
/// The HTTP half of the criterion is "one request served **or** 120 s of
/// uptime". A device on a shelf with nothing polling it is not broken, and a
/// criterion that required a visitor would revert every honest update on a quiet
/// network.
pub const HTTP_GRACE_MS: u32 = 120_000;

/// An image that has not confirmed by now is reverted.
pub const REVERT_AT_MS: u32 = 180_000;

const _: () = assert!(CONFIRM_NOT_BEFORE_MS < HTTP_GRACE_MS);
const _: () = assert!(HTTP_GRACE_MS < REVERT_AT_MS);

/// What the device can say about itself, for [`decide`].
///
/// Four numbers and a flag, all of which the firmware already has in an atomic
/// somewhere. Nothing here is OTA-specific: they are "is it on the network",
/// "has anybody talked to it", "is the panel alive", which is what research 006
/// section 6 means by healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Health {
    /// Milliseconds since this boot.
    pub uptime_ms: u32,
    /// WiFi is associated **and** DHCP has handed out an address. One flag for
    /// both because the firmware only ever learns the second, and the second
    /// implies the first.
    pub has_ip: bool,
    /// HTTP requests served since boot.
    pub http_requests: u32,
    /// Panel swaps since boot.
    pub swaps: u32,
}

impl Health {
    /// Is every part of research 006 section 6's criterion met *now*?
    ///
    /// Ignores [`CONFIRM_NOT_BEFORE_MS`] - that is a separate question and
    /// [`decide`] asks both.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.has_ip
            && self.swaps > 0
            && (self.http_requests > 0 || self.uptime_ms >= HTTP_GRACE_MS)
    }
}

/// What the image on trial should do about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialAction {
    /// Not yet, either way.
    Wait,
    /// Mark the running slot `VALID`. Exactly once.
    Confirm,
    /// Mark it `INVALID` and reset; the bootloader then boots the other slot.
    Revert,
}

/// The whole decision, for an image that is on trial.
///
/// **Confirm is tested before revert**, so a device that becomes healthy on the
/// same tick the deadline lands keeps the update. That is the safe direction:
/// the alternative throws away an image that was demonstrably working, and the
/// reverted one is only ever "the version before", not "a version known to be
/// better".
#[must_use]
pub fn decide(h: Health) -> TrialAction {
    if h.uptime_ms >= CONFIRM_NOT_BEFORE_MS && h.is_healthy() {
        TrialAction::Confirm
    } else if h.uptime_ms >= REVERT_AT_MS {
        TrialAction::Revert
    } else {
        TrialAction::Wait
    }
}

// ---------------------------------------------------------------------------
// What kind of boot this is
// ---------------------------------------------------------------------------

/// What this boot is, as far as the update machinery is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    /// Nothing to do: the image the bootloader selected is running and its
    /// `otadata` entry already says `VALID`. Every boot of a serial-flashed
    /// device, and every boot after a confirmed update. **No `otadata` write
    /// happens on one of these**, which is the point of telling it apart.
    Settled,
    /// On trial: confirm it or revert it. [`decide`] says which.
    Trial,
    /// The running slot is the selected one, but its entry was never marked, so
    /// the bootloader did not put it on trial. That is the signature of an
    /// activation interrupted between its two writes (card 241's interruption
    /// 5b). Mark it `PENDING_VERIFY` and treat it as [`Boot::Trial`]: an image
    /// nobody vouched for must not become permanent just because the power went
    /// out at the wrong millisecond.
    Unproven,
    /// The bootloader is not running what `otadata` selected: the last
    /// activation was rolled back and this is the image before it.
    Reverted(RevertReason),
    /// The MMU or `otadata` could not be read. Report it and change nothing: a
    /// device that cannot say which slot it is running is the last device that
    /// should be writing to `otadata`.
    Unknown,
}

impl Boot {
    /// Does this boot need the trial machinery - the deadline, the watchdog and
    /// eventually one `otadata` write?
    #[must_use]
    pub fn on_trial(&self) -> bool {
        matches!(self, Boot::Trial | Boot::Unproven)
    }

    /// The reason, for a boot that came back from a rolled-back update.
    #[must_use]
    pub fn revert_reason(&self) -> Option<RevertReason> {
        match self {
            Boot::Reverted(r) => Some(*r),
            _ => None,
        }
    }
}

/// Decide what kind of boot this is.
///
/// `booted` is the MMU's answer (`PartitionTable::booted_partition`), `selected`
/// is `otadata`'s (`Ota::current_app_partition`) and `state` is the selected
/// entry's (`Ota::current_ota_state`).
///
/// **The disagreement is the detection.** `esp-bootloader-esp-idf`'s
/// `current_app_partition` works from `max(ota_seq)` alone and does not look at
/// the states, so after the bootloader has rolled an update back it still names
/// the slot that was rolled back *from*. The MMU names the one really running.
/// When the two differ, something the app did not decide has happened, and the
/// selected entry's state says what: `INVALID` is this firmware giving up at the
/// deadline, `ABORTED` is the bootloader reclaiming a trial that never
/// confirmed, and anything else is the bootloader refusing an image it could not
/// verify.
///
/// `New` is treated as a trial although the bootloader should have promoted it
/// to `PENDING_VERIFY` before handing over: seeing it means a bootloader without
/// `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`, and on such a device the app-side
/// deadline is the only protection there is - so run it.
#[must_use]
pub fn classify(booted: FwSlot, selected: FwSlot, state: FwState) -> Boot {
    let (FwSlot::Ota0 | FwSlot::Ota1) = booted else {
        return Boot::Unknown;
    };
    let (FwSlot::Ota0 | FwSlot::Ota1) = selected else {
        return Boot::Unknown;
    };
    if booted != selected {
        return Boot::Reverted(reason_for(state));
    }
    match state {
        FwState::PendingVerify | FwState::New => Boot::Trial,
        FwState::Undefined => Boot::Unproven,
        FwState::Valid => Boot::Settled,
        // The bootloader will not select an `INVALID` or `ABORTED` entry, so
        // reaching here means both entries were unusable and it fell back to
        // `ota_0` ("No factory image, trying OTA 0"), which happened to be the
        // slot the sequence arithmetic also points at. `otadata` is in a mess,
        // but the device is running and the honest thing to report is that the
        // last update did not stick.
        FwState::Invalid => Boot::Reverted(RevertReason::Deadline),
        FwState::Aborted => Boot::Reverted(RevertReason::Aborted),
    }
}

fn reason_for(state: FwState) -> RevertReason {
    match state {
        FwState::Invalid => RevertReason::Deadline,
        FwState::Aborted => RevertReason::Aborted,
        _ => RevertReason::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- classify ---------------------------------------------------------

    #[test]
    fn a_serial_flashed_device_is_settled_and_writes_nothing() {
        // What `tools/fw-run.sh` leaves behind: otadata erased, so the
        // bootloader writes seq 1 VALID for ota_0 and boots it.
        assert_eq!(
            classify(FwSlot::Ota0, FwSlot::Ota0, FwState::Valid),
            Boot::Settled
        );
        assert!(!Boot::Settled.on_trial());
    }

    #[test]
    fn the_first_boot_of_an_activated_image_is_a_trial() {
        assert_eq!(
            classify(FwSlot::Ota1, FwSlot::Ota1, FwState::PendingVerify),
            Boot::Trial
        );
    }

    #[test]
    fn new_that_the_bootloader_never_promoted_is_still_a_trial() {
        // A bootloader without APP_ROLLBACK_ENABLE. The app-side deadline is
        // then the only thing standing between a bad image and a permanent one.
        assert_eq!(
            classify(FwSlot::Ota1, FwSlot::Ota1, FwState::New),
            Boot::Trial
        );
    }

    #[test]
    fn an_activation_interrupted_between_its_two_writes_is_unproven_not_settled() {
        // Interruption 5b: the sequence was written, the state was not.
        let b = classify(FwSlot::Ota1, FwSlot::Ota1, FwState::Undefined);
        assert_eq!(b, Boot::Unproven);
        assert!(b.on_trial(), "an image nobody vouched for must still be tried");
    }

    #[test]
    fn booted_is_not_selected_means_the_update_was_rolled_back() {
        assert_eq!(
            classify(FwSlot::Ota0, FwSlot::Ota1, FwState::Aborted),
            Boot::Reverted(RevertReason::Aborted)
        );
        assert_eq!(
            classify(FwSlot::Ota0, FwSlot::Ota1, FwState::Invalid),
            Boot::Reverted(RevertReason::Deadline)
        );
        // Neither: the bootloader could not verify the image it selected.
        assert_eq!(
            classify(FwSlot::Ota0, FwSlot::Ota1, FwState::Valid),
            Boot::Reverted(RevertReason::Rejected)
        );
    }

    #[test]
    fn a_device_that_cannot_say_which_slot_it_runs_changes_nothing() {
        for state in [FwState::PendingVerify, FwState::Valid, FwState::Undefined] {
            assert_eq!(classify(FwSlot::Unknown, FwSlot::Ota0, state), Boot::Unknown);
            assert_eq!(classify(FwSlot::Ota0, FwSlot::Unknown, state), Boot::Unknown);
        }
        assert!(!Boot::Unknown.on_trial());
    }

    // --- decide -----------------------------------------------------------

    fn healthy_at(uptime_ms: u32) -> Health {
        Health {
            uptime_ms,
            has_ip: true,
            http_requests: 1,
            swaps: 1,
        }
    }

    #[test]
    fn nothing_confirms_before_sixty_seconds_however_healthy() {
        for t in [0, 1_000, 30_000, CONFIRM_NOT_BEFORE_MS - 1] {
            assert_eq!(decide(healthy_at(t)), TrialAction::Wait, "at {t} ms");
        }
        assert_eq!(
            decide(healthy_at(CONFIRM_NOT_BEFORE_MS)),
            TrialAction::Confirm
        );
    }

    #[test]
    fn every_part_of_the_criterion_is_needed() {
        let base = healthy_at(CONFIRM_NOT_BEFORE_MS);
        assert_eq!(decide(base), TrialAction::Confirm);
        assert_eq!(
            decide(Health {
                has_ip: false,
                ..base
            }),
            TrialAction::Wait,
            "no DHCP address"
        );
        assert_eq!(
            decide(Health { swaps: 0, ..base }),
            TrialAction::Wait,
            "the panel never swapped"
        );
        assert_eq!(
            decide(Health {
                http_requests: 0,
                ..base
            }),
            TrialAction::Wait,
            "nobody has talked to it, and it is not 120 s yet"
        );
    }

    #[test]
    fn a_device_nobody_visits_confirms_at_two_minutes() {
        let quiet = Health {
            uptime_ms: HTTP_GRACE_MS - 1,
            has_ip: true,
            http_requests: 0,
            swaps: 1,
        };
        assert_eq!(decide(quiet), TrialAction::Wait);
        assert_eq!(
            decide(Health {
                uptime_ms: HTTP_GRACE_MS,
                ..quiet
            }),
            TrialAction::Confirm
        );
    }

    #[test]
    fn an_image_that_never_becomes_healthy_is_reverted_at_three_minutes() {
        let sick = Health {
            uptime_ms: REVERT_AT_MS - 1,
            has_ip: false,
            http_requests: 0,
            swaps: 1,
        };
        assert_eq!(decide(sick), TrialAction::Wait);
        assert_eq!(
            decide(Health {
                uptime_ms: REVERT_AT_MS,
                ..sick
            }),
            TrialAction::Revert
        );
    }

    #[test]
    fn health_arriving_on_the_deadline_tick_keeps_the_update() {
        assert_eq!(decide(healthy_at(REVERT_AT_MS)), TrialAction::Confirm);
        assert_eq!(decide(healthy_at(REVERT_AT_MS + 60_000)), TrialAction::Confirm);
    }
}
