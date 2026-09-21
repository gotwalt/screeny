//! Waiting out a firmware update, correctly (card 246, item 2).
//!
//! `screeny-probe fw-upload --activate` sends an image, gets
//! `{"ok":true,"activating":true}` and then has to answer one question: *did
//! it stick?* On the bench it got that wrong in the most misleading way
//! possible. The device answers the upload **about two seconds before it
//! restarts** (`ota::ACTIVATE_DELAY_MS`, which exists so the reply is
//! acknowledged before the connection is destroyed), so the first poll after
//! the reply reaches the **old image**, still running, still answering. The
//! probe printed
//!
//! ```text
//! back after 28 s: fw 0.7.0 slot Ota0 state Valid ... uptime 148180 ms
//! the device reports no update record - nothing to wait for
//! ```
//!
//! and exited `0` while the panel was rebooting into its trial. Both lines are
//! the same mistake: a device that has not gone away yet has the *old* state,
//! and the old state has nothing to say about an update that has not started.
//!
//! The fix is to wait for something that cannot be true of the old image.
//! `boot_id` is exactly that - a random `u32` drawn once per boot, which the
//! API added for this (`device-web.md`, card 226's paragraph) - so the wait is:
//!
//! 1. read `boot_id` **before** the upload;
//! 2. poll `GET /api/v1/status` until it **changes**, ignoring every answer
//!    that still carries the old one;
//! 3. then poll `GET /api/v1/panic` until `update.outcome` leaves `trial`.
//!
//! Both phases are bounded, as they were, and neither can end early by
//! mistaking one image for the other. `crates/sim`'s [`ota`] model plays the
//! whole sequence - old image, away, trial, confirmed - so this is tested
//! without hardware.
//!
//! [`ota`]: https://docs.rs/screeny-sim

use std::time::{Duration, Instant};

use screeny_device_api::reply::{PanicReply, StatusReply};
use screeny_device_api::{route, UpdateOutcome};

use super::client::Client;

/// How patient the wait is, and how often it asks.
///
/// The defaults are the device's own numbers with room to spare: a boot to a
/// DHCP address is ~20 s, a trial confirms between 60 and 120 s and reverts at
/// 180 s, and the old image answers for ~2 s after the reply.
#[derive(Debug, Clone, Copy)]
pub struct Watch {
    /// How long to wait for a **different** `boot_id` to answer.
    pub reappear: Duration,
    /// How long to wait after that for the trial to end.
    pub decide: Duration,
    /// How long between polls.
    pub poll: Duration,
    /// How long to put up with a device that is back but reports no `update`
    /// record before calling it a firmware that predates card 241.
    ///
    /// **Not zero, and that is the bug this exists to not repeat**: the record
    /// appears when the new image classifies its own boot, and a poll that
    /// lands in the second before that must wait rather than conclude.
    pub no_record_grace: Duration,
}

impl Default for Watch {
    fn default() -> Self {
        Watch {
            reappear: Duration::from_secs(90),
            decide: Duration::from_secs(240),
            poll: Duration::from_secs(1),
            no_record_grace: Duration::from_secs(20),
        }
    }
}

/// What the wait saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The new image confirmed itself. The update stuck.
    Confirmed,
    /// The device rolled back to the image before it.
    Reverted,
    /// No different `boot_id` ever answered: either it never restarted, or it
    /// restarted and never came back.
    NeverCameBack,
    /// It came back, and reports no `update` record at all: a firmware without
    /// card 241, or a device running something that was never activated.
    NoRecord,
    /// It came back and the trial had not ended when patience ran out.
    Undecided,
}

impl Outcome {
    /// Whether the tool should exit zero.
    #[must_use]
    pub fn ok(self) -> bool {
        matches!(self, Outcome::Confirmed | Outcome::NoRecord)
    }
}

/// Wait out an activating upload and say what became of it.
///
/// `before` is the `boot_id` read **before** the upload; `None` means it could
/// not be read, and then this can only wait for *an* answer, which is what
/// firmware 0.7.0's bench run did by accident - so it says so.
///
/// `say` is every line this would have printed, so the caller can print them
/// as they happen and a test can read them afterwards.
pub fn watch(client: &Client, before: Option<u32>, w: &Watch, say: &mut dyn FnMut(String)) -> Outcome {
    let t0 = Instant::now();
    if before.is_none() {
        say(
            "  the device's boot_id could not be read before the upload, so \"it came back\" \
             here means \"something answered\" and may be the old image still running"
                .into(),
        );
    }

    // --- 1: wait for a boot_id that is not the one we started with ---------
    say(format!(
        "  waiting for the device to restart (up to {:.0} s)...",
        w.reappear.as_secs_f32()
    ));
    let deadline = Instant::now() + w.reappear;
    let mut back: Option<StatusReply> = None;
    let mut said_old = false;
    while Instant::now() < deadline {
        std::thread::sleep(w.poll);
        let Ok(res) = client.get(route::STATUS) else {
            continue;
        };
        let Ok(s) = res.parse::<StatusReply>() else {
            continue;
        };
        if before == Some(s.boot_id) {
            // The activation happens ~2 s after the reply: this is the old
            // image, and everything it says is about the old image.
            if !said_old {
                said_old = true;
                say(format!(
                    "  at {:.0} s: still boot_id {} - the old image, which has not restarted yet",
                    t0.elapsed().as_secs_f64(),
                    s.boot_id
                ));
            }
            continue;
        }
        say(format!(
            "  back after {:.0} s: fw {} slot {:?} state {:?} boot_id {} (was {:?}) uptime {} ms",
            t0.elapsed().as_secs_f64(),
            s.fw,
            s.fw_slot,
            s.fw_state,
            s.boot_id,
            before,
            s.uptime_ms,
        ));
        back = Some(s);
        break;
    }
    if back.is_none() {
        say(format!(
            "  no boot_id but {:?} answered within {:.0} s. Either the activation never \
             happened, or the device is still rebooting, or the update is panicking and being \
             rolled back; try `screeny-probe status` in a minute, and `tools/fw-run.sh` if it \
             never comes back.",
            before,
            w.reappear.as_secs_f32()
        ));
        return Outcome::NeverCameBack;
    }

    // --- 2: wait for the trial to end --------------------------------------
    say(format!(
        "  waiting for the trial to end (up to {:.0} s)...",
        w.decide.as_secs_f32()
    ));
    let came_back = Instant::now();
    let deadline = came_back + w.decide;
    let mut last = String::new();
    while Instant::now() < deadline {
        std::thread::sleep(w.poll);
        let Ok(res) = client.get(route::PANIC) else {
            continue;
        };
        let Ok(p) = res.parse::<PanicReply>() else {
            continue;
        };
        let Some(u) = p.update else {
            // The new image is up but has not written its record yet, or this
            // firmware has none. Only the second is a reason to stop, and only
            // after the grace.
            if came_back.elapsed() < w.no_record_grace {
                continue;
            }
            say(format!(
                "  the device has been back {:.0} s and reports no update record. That is a \
                 firmware without card 241, or an image that was never activated - there is \
                 nothing here to wait for.",
                came_back.elapsed().as_secs_f64()
            ));
            return Outcome::NoRecord;
        };
        let now = format!("{:?}", u.outcome);
        if now != last {
            say(format!(
                "  at {:.0} s: {:?} slot {:?} version {:?} reason {:?}",
                t0.elapsed().as_secs_f64(),
                u.outcome,
                u.slot,
                u.version.as_deref(),
                u.reason,
            ));
            last = now;
        }
        match u.outcome {
            UpdateOutcome::Trial => {}
            UpdateOutcome::Confirmed => {
                say("  CONFIRMED: the update stuck.".into());
                return Outcome::Confirmed;
            }
            UpdateOutcome::Reverted => {
                say(
                    "  REVERTED: the update did not stick and the previous image is running. \
                     GET /api/v1/panic has the panic record, if there is one."
                        .into(),
                );
                return Outcome::Reverted;
            }
        }
    }
    say(format!(
        "  the trial had not ended after {:.0} s - report that, it should not happen",
        w.decide.as_secs_f32()
    ));
    Outcome::Undecided
}

/// The `boot_id` the device is running under right now, for the caller to hold
/// across an upload.
///
/// `None` for anything that stops it being read - which is not an error here:
/// a device that cannot be read before the upload can still be waited for,
/// less precisely, and [`watch`] says so.
#[must_use]
pub fn boot_id(client: &Client) -> Option<u32> {
    client
        .get(route::STATUS)
        .ok()?
        .parse::<StatusReply>()
        .ok()
        .map(|s| s.boot_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_confirm_and_a_missing_record_are_successes() {
        assert!(Outcome::Confirmed.ok());
        assert!(Outcome::NoRecord.ok());
        assert!(!Outcome::Reverted.ok());
        assert!(!Outcome::NeverCameBack.ok());
        assert!(!Outcome::Undecided.ok());
    }

    #[test]
    fn the_defaults_are_the_devices_own_numbers_with_room() {
        let w = Watch::default();
        // The firmware reverts at 180 s and needs ~20 s to boot the old image.
        assert!(w.decide >= Duration::from_secs(200));
        // A boot to a DHCP address is ~20 s.
        assert!(w.reappear >= Duration::from_secs(60));
        // **Not zero**: that was the bug.
        assert!(w.no_record_grace >= Duration::from_secs(10));
    }
}
