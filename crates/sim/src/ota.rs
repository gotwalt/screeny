//! A paper firmware update: activate, go away, come back on trial, confirm.
//!
//! **Off unless a test asks for it** ([`SimHandle::model_ota`]), and that is
//! the point of it. The simulator has one slot and it is the running binary,
//! so `POST /api/v1/firmware` has always answered `activating: false` - a
//! simulator that claimed to be rebooting into an image it threw away would be
//! the one thing a simulator must never be. What card 246 needs is not a
//! second answer to that question but a **clock**: something that plays the
//! three seconds in which a real device has replied and has *not yet gone
//! away*, because that is the race `screeny-probe fw-upload --activate` lost
//! on the bench. It read `GET /api/v1/status`, saw the old image still
//! answering with its old `boot_id` and 148 s of uptime, called that "the
//! device came back", found no update record and declared victory while the
//! panel was in fact rebooting into its trial.
//!
//! So this models exactly what the device does between the reply and the
//! confirm, and nothing else:
//!
//! | from | until | what a client sees |
//! |---|---|---|
//! | the reply | `old_image_for` | the **old** `boot_id`, the old version, no update record - `ota::activate_task` has not written `otadata` yet |
//! | then | `+ away_for` | nothing at all: connections are closed without an answer, as a rebooting device does |
//! | then | `+ trial_for` | a **new** `boot_id`, the uploaded version, `panic.update.outcome = trial` |
//! | then | for ever | the same, `outcome = confirmed`, `fw_state valid` |
//!
//! A revert is not modelled: what the probe has to get right is the waiting,
//! and the two ends of a trial are one field apart. When something needs a
//! reverted device, this is where to add it.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use screeny_device_api::reply::UpdateRecord;
use screeny_device_api::{FwSlot, FwState, UpdateOutcome};

/// How long each phase lasts.
///
/// The defaults are a bench device's: `ota::ACTIVATE_DELAY_MS` is 2 s, a boot
/// to a DHCP address is ~15 s, and a trial confirms between 60 s and 120 s. A
/// test sets its own, in hundreds of milliseconds, because what it is testing
/// is an order of events and not a duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtaTiming {
    /// How long the old image keeps answering after the reply.
    pub old_image_for: Duration,
    /// How long the device is unreachable.
    pub away_for: Duration,
    /// How long the new image is on trial before it confirms itself.
    pub trial_for: Duration,
}

impl Default for OtaTiming {
    fn default() -> Self {
        OtaTiming {
            old_image_for: Duration::from_secs(2),
            away_for: Duration::from_secs(15),
            trial_for: Duration::from_secs(60),
        }
    }
}

/// Where a modelled update has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No update has been activated.
    Idle,
    /// The reply is out; the old image is still the one answering.
    OldImage,
    /// Rebooting: nothing answers.
    Away,
    /// The new image is up and on trial.
    Trial,
    /// It confirmed itself.
    Confirmed,
}

/// The model, as the simulator holds it: one lock, and nothing in it until a
/// test turns it on.
#[derive(Debug, Default)]
pub struct OtaModel {
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    /// `None` means the whole model is off and `POST /api/v1/firmware` answers
    /// `activating: false` as it always has.
    timing: Option<OtaTiming>,
    /// When the activating upload was answered.
    started: Option<Instant>,
    /// The `boot_id` the new image comes up with. Drawn when the upload is
    /// accepted, so a test can read it before the device has gone away.
    new_boot_id: u32,
    /// `esp_app_desc.version` of the image that was uploaded.
    version: String,
    /// The slot it went into - the simulator's `Ident::fw_slot` is `ota_0` by
    /// default, so an update lands in the other one, exactly as a device's
    /// does.
    slot: FwSlot,
}

impl Default for State {
    fn default() -> Self {
        State {
            timing: None,
            started: None,
            new_boot_id: 0,
            version: String::new(),
            // Overwritten by every `activate`; `FwSlot` has no `Default` and
            // inventing one for it would be a wire-facing enum growing a
            // meaning for the sake of this file.
            slot: FwSlot::Ota1,
        }
    }
}

impl OtaModel {
    /// A model that is off.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Turn it on (or, with `None`, off again), and forget any update in
    /// flight.
    pub fn set(&self, timing: Option<OtaTiming>) {
        let mut s = self.lock();
        s.timing = timing;
        s.started = None;
    }

    /// Is an activating upload going to be played out?
    #[must_use]
    pub fn is_on(&self) -> bool {
        self.lock().timing.is_some()
    }

    /// An upload was accepted and asked to activate. Returns whether the model
    /// took it, which is what the reply's `activating` says.
    ///
    /// `running` is the slot the simulator says it is running, so that the
    /// update lands in the other one.
    pub fn activate(&self, version: &str, running: FwSlot) -> bool {
        let mut s = self.lock();
        if s.timing.is_none() {
            return false;
        }
        s.started = Some(Instant::now());
        // Not random: a simulator that drew a random id would make a failing
        // test print a different number every run. It has to *differ* from the
        // one before, and one more than it does that and reads as a restart.
        s.new_boot_id = s.new_boot_id.wrapping_add(1);
        s.version = version.to_string();
        s.slot = match running {
            FwSlot::Ota0 => FwSlot::Ota1,
            _ => FwSlot::Ota0,
        };
        true
    }

    /// Where the update has got to now.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase_at(Instant::now())
    }

    /// The same, at a given instant, which is what the unit tests drive.
    fn phase_at(&self, now: Instant) -> Phase {
        let s = self.lock();
        let (Some(t), Some(started)) = (s.timing, s.started) else {
            return Phase::Idle;
        };
        let since = now.saturating_duration_since(started);
        if since < t.old_image_for {
            Phase::OldImage
        } else if since < t.old_image_for + t.away_for {
            Phase::Away
        } else if since < t.old_image_for + t.away_for + t.trial_for {
            Phase::Trial
        } else {
            Phase::Confirmed
        }
    }

    /// What `GET /api/v1/status` should say about the running image, or `None`
    /// when the model has nothing to say and the simulator's own values stand.
    ///
    /// Three fields and they move together, because they are three facts about
    /// one image: a reader that saw a new `boot_id` beside the old version
    /// would be reading something no device can produce.
    #[must_use]
    pub fn running_image(&self) -> Option<RunningImage> {
        // **The phase first, and the lock once.** `Mutex` is not reentrant:
        // asking `phase()` again with the guard in hand deadlocks the HTTP
        // thread that is holding it, and every request after it.
        let phase = self.phase();
        match phase {
            // Before the reboot the old image is still answering, and it
            // answers with its own numbers - which is the whole trap.
            Phase::Idle | Phase::OldImage | Phase::Away => None,
            Phase::Trial | Phase::Confirmed => {
                let s = self.lock();
                Some(RunningImage {
                    boot_id: s.new_boot_id,
                    version: s.version.clone(),
                    slot: s.slot,
                    // Card 246 item 3: the running slot's own state, which
                    // during a trial is `pending_verify` and after the confirm
                    // is `valid`.
                    state: if phase == Phase::Confirmed {
                        FwState::Valid
                    } else {
                        FwState::PendingVerify
                    },
                })
            }
        }
    }

    /// `GET /api/v1/panic`'s `update`.
    #[must_use]
    pub fn update_record(&self) -> Option<UpdateRecord> {
        let outcome = match self.phase() {
            Phase::Idle | Phase::OldImage | Phase::Away => return None,
            Phase::Trial => UpdateOutcome::Trial,
            Phase::Confirmed => UpdateOutcome::Confirmed,
        };
        let s = self.lock();
        Some(UpdateRecord {
            outcome,
            reason: None,
            slot: s.slot,
            version: screeny_device_api::text::text(&s.version),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// What the model says is running, for `GET /api/v1/status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningImage {
    /// The `boot_id` of the image on trial.
    pub boot_id: u32,
    /// Its `esp_app_desc.version`.
    pub version: String,
    /// The slot it is in.
    pub slot: FwSlot,
    /// That slot's `otadata` state.
    pub state: FwState,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(t: OtaTiming) -> OtaModel {
        let m = OtaModel::new();
        m.set(Some(t));
        m
    }

    const FAST: OtaTiming = OtaTiming {
        old_image_for: Duration::from_millis(100),
        away_for: Duration::from_millis(100),
        trial_for: Duration::from_millis(100),
    };

    #[test]
    fn a_model_that_is_off_takes_no_activation_and_says_nothing() {
        let m = OtaModel::new();
        assert!(!m.is_on());
        assert!(!m.activate("9.9.9", FwSlot::Ota0));
        assert_eq!(m.phase(), Phase::Idle);
        assert!(m.update_record().is_none());
        assert!(m.running_image().is_none());
    }

    #[test]
    fn the_old_image_answers_first_and_has_no_update_record() {
        let m = model(FAST);
        assert!(m.activate("0.7.2", FwSlot::Ota0));
        // The instant after the reply: this is where the probe used to decide.
        assert_eq!(m.phase(), Phase::OldImage);
        assert!(
            m.running_image().is_none(),
            "the old image answers with its own boot_id"
        );
        assert!(m.update_record().is_none(), "nothing has been activated yet");
    }

    #[test]
    fn the_phases_run_in_order_and_end_confirmed() {
        let m = model(FAST);
        let t0 = Instant::now();
        m.activate("0.7.2", FwSlot::Ota0);
        assert_eq!(m.phase_at(t0), Phase::OldImage);
        assert_eq!(m.phase_at(t0 + Duration::from_millis(150)), Phase::Away);
        assert_eq!(m.phase_at(t0 + Duration::from_millis(250)), Phase::Trial);
        assert_eq!(m.phase_at(t0 + Duration::from_millis(350)), Phase::Confirmed);
        assert_eq!(m.phase_at(t0 + Duration::from_secs(3600)), Phase::Confirmed);
    }

    #[test]
    fn the_image_that_comes_back_is_the_one_that_was_uploaded() {
        let m = model(OtaTiming {
            old_image_for: Duration::ZERO,
            away_for: Duration::ZERO,
            trial_for: Duration::from_secs(60),
        });
        m.activate("0.7.2", FwSlot::Ota0);
        let r = m.running_image().expect("on trial");
        assert_eq!(r.version, "0.7.2");
        assert_eq!(r.slot, FwSlot::Ota1, "an update lands in the other slot");
        assert_eq!(r.state, FwState::PendingVerify);
        let u = m.update_record().expect("a record");
        assert_eq!(u.outcome, UpdateOutcome::Trial);
        assert_eq!(u.version.as_deref(), Some("0.7.2"));
        assert_eq!(u.slot, FwSlot::Ota1);
    }

    #[test]
    fn the_boot_id_changes_with_every_update() {
        let m = model(FAST);
        m.set(Some(OtaTiming {
            old_image_for: Duration::ZERO,
            away_for: Duration::ZERO,
            trial_for: Duration::from_secs(60),
        }));
        m.activate("a", FwSlot::Ota0);
        let first = m.running_image().expect("on trial").boot_id;
        m.activate("b", FwSlot::Ota1);
        let second = m.running_image().expect("on trial").boot_id;
        assert_ne!(first, second);
    }
}
