//! The radio the simulator does not have, scripted.
//!
//! Card 081's deliverables, delivered as part of card 224: a join outcome a
//! test or the CLI chooses, a link a test can take down, and the whole
//! `Boot -> Joining -> Online | Portal -> Trial` life of research 007 section
//! 5.2 - driven by [`screeny_provision::Provisioner`], which is the same state
//! machine the firmware (card 223) drives. **The machine is not restated
//! here.** This module supplies the two things a `no_std`, clockless,
//! radio-less crate cannot: a clock, and something that answers a join.
//!
//! # What "scripted" means
//!
//! The machine emits [`Action::StartJoin`]; a real device would hand that to
//! `esp-radio` and wait for an event. Here, [`WifiOutcome`] decides what comes
//! back and [`WifiModel::tick`] delivers it after
//! [`Config::wifi_join_ms`](crate::Config::wifi_join_ms):
//!
//! | outcome | what the scripted radio does |
//! |---|---|
//! | [`WifiOutcome::Ok`] | answers [`Event::Joined`] |
//! | [`WifiOutcome::Fail`] | answers [`Event::JoinFailed`] with [`FailReason::AuthError`] |
//! | [`WifiOutcome::Slow`] | answers nothing at all, so the attempt runs into the machine's own `join_attempt_ms` timeout and fails with [`FailReason::Other`] |
//!
//! The **boot** join is the exception: with [`WifiOutcome::Ok`] it completes at
//! time zero rather than after `wifi_join_ms`, so the default simulator is
//! `Online` from its first instant and answers `GET_WIFI` exactly as it always
//! has. Every later join - a trial, a retry, a rejoin after the link came
//! back - takes the scripted time.
//!
//! # The PSK is not here either
//!
//! [`screeny_provision`] has no field for a password and neither does this
//! module. [`WifiModel::post_credentials`] takes the SSID and the PSK's
//! *length*; the bytes are dropped by the caller before this is reached.
//! Spec section 8.4's invariant is therefore structural in the simulator too,
//! and `tests/http_wifi.rs` greps for it rather than trusting the claim.

use std::net::Ipv4Addr;

use screeny_device_api::reply::WifiReply;
use screeny_provision::machine::{Config as ProvConfig, Trial};
use screeny_provision::{Action, Event as ProvEvent, JoinTarget, Provisioner, Screen, State};

use crate::config::Config;
use crate::event::Event;

pub use screeny_provision::machine::FailReason as WifiFailReason;
pub use screeny_provision::machine::Timing as WifiTiming;
pub use screeny_provision::State as WifiPhase;

/// What the scripted radio does with a join attempt. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WifiOutcome {
    /// Every attempt succeeds.
    #[default]
    Ok,
    /// Every attempt fails with a wrong password.
    Fail,
    /// The radio never answers, so the attempt times out.
    Slow,
}

impl WifiOutcome {
    /// Parse the `--wifi-result` value.
    ///
    /// # Errors
    /// The string, if it is not one of the three.
    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "ok" => WifiOutcome::Ok,
            "fail" => WifiOutcome::Fail,
            "slow" => WifiOutcome::Slow,
            other => return Err(format!("unknown wifi result {other:?}")),
        })
    }

    /// The name `--wifi-result` uses.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            WifiOutcome::Ok => "ok",
            WifiOutcome::Fail => "fail",
            WifiOutcome::Slow => "slow",
        }
    }
}

/// What happened to a posted credential.
///
/// Card 224 saw `Ignored` for every post that arrived while the device was
/// `Online`, which is exactly the case card 223's LAN settings page is. Card
/// 232 gave the machine that transition, so a post now starts a trial from
/// `Portal`, `Trial`, `Joining` **and** `Online`, and `Ignored` is left for
/// the one state that cannot receive one: `Boot`, before `Event::Boot` has
/// been processed. The simulator never observes that state - it boots the
/// machine inside [`WifiModel::new`] - so the arm is kept for honesty rather
/// than because it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posted {
    /// A trial join started.
    Trial,
    /// The machine had no transition for it in this state.
    Ignored(WifiPhase),
}

/// The scripted radio and the provisioning machine it drives.
///
/// `Debug` is safe to log: there is no credential in it, because there is no
/// field for one.
#[derive(Debug)]
pub struct WifiModel {
    p: Provisioner,
    outcome: WifiOutcome,
    join_ms: u32,
    /// When the scripted radio answers the attempt in flight, and with what.
    pending: Option<u32>,
    link_down: bool,
    /// The SSID the store holds. **Never a PSK**: nothing here has a field
    /// for one.
    stored_ssid: Option<String>,
    /// How many times the store has been written. A test asserts this does
    /// not move when a trial fails.
    commits: u32,
    ip: Ipv4Addr,
}

impl WifiModel {
    /// Build the model and boot the machine.
    #[must_use]
    pub fn new(cfg: &Config, ip: Ipv4Addr, out: &mut Vec<Event>) -> Self {
        let stored = if cfg.start_in_portal {
            None
        } else {
            Some(cfg.wifi_ssid.clone())
        };
        let mut m = WifiModel {
            p: Provisioner::new(&ProvConfig {
                ap_ssid: &cfg.ap_ssid,
                has_stored: stored.is_some(),
                form: screeny_provision::UriForm::NoPass,
                timing: cfg.wifi_timing,
            }),
            outcome: cfg.wifi_outcome,
            join_ms: cfg.wifi_join_ms,
            pending: None,
            link_down: false,
            stored_ssid: stored,
            commits: 0,
            ip,
        };
        m.step(ProvEvent::Boot, 0, out);
        // The boot join, and only the boot join, is instant when it is going
        // to succeed: the default simulator has always claimed to be on the
        // network from the moment it started, and a 200 ms window of
        // `CONNECTING` at startup would be a behaviour change nobody asked
        // for. See the module docs.
        if m.outcome == WifiOutcome::Ok && m.pending.is_some() {
            m.pending = None;
            m.step(ProvEvent::Joined { ip: m.ip.octets() }, 0, out);
        }
        m
    }

    // --- queries -----------------------------------------------------------

    /// Where the provisioning machine is.
    #[must_use]
    pub fn phase(&self) -> WifiPhase {
        self.p.state()
    }

    /// The `GET_WIFI` state byte (spec section 6.3), straight from the machine.
    #[must_use]
    pub fn wifi_state(&self) -> u8 {
        self.p.wifi_state()
    }

    /// The telemetry `state` byte overlay: `PROVISIONING` while the portal or
    /// a trial is up, `None` otherwise.
    #[must_use]
    pub fn overlay_state(&self) -> Option<u8> {
        self.p.overlay_state()
    }

    /// The SSID `GET_WIFI` and `/api/v1/status` report: the trial's while one
    /// is in flight, the stored one otherwise. Empty when there is none.
    #[must_use]
    pub fn ssid(&self) -> &str {
        match (self.p.state(), self.p.trial()) {
            (State::Trial, Some(t)) => t.ssid.as_str(),
            _ => self.stored_ssid.as_deref().unwrap_or(""),
        }
    }

    /// The address the station holds, if it is on a network.
    #[must_use]
    pub fn ip(&self) -> Option<[u8; 4]> {
        self.p.ip()
    }

    /// The last trial, for `GET /api/v1/wifi`.
    #[must_use]
    pub fn trial(&self) -> Option<&Trial> {
        self.p.trial()
    }

    /// Whether the soft-AP is up.
    #[must_use]
    pub fn ap_up(&self) -> bool {
        self.p.ap_up()
    }

    /// The soft-AP's name.
    #[must_use]
    pub fn ap_ssid(&self) -> &str {
        self.p.ap_ssid()
    }

    /// The SSID the store holds. There is no method for the PSK because there
    /// is no PSK: the simulator never keeps one.
    #[must_use]
    pub fn stored_ssid(&self) -> Option<&str> {
        self.stored_ssid.as_deref()
    }

    /// How many times the store has been written since boot.
    #[must_use]
    pub fn commits(&self) -> u32 {
        self.commits
    }

    /// Whether a test has taken the link down.
    #[must_use]
    pub fn link_is_down(&self) -> bool {
        self.link_down
    }

    /// True when the device is on a network and nothing is being provisioned.
    #[must_use]
    pub fn online(&self) -> bool {
        self.p.state() == State::Online && !self.link_down
    }

    /// The outcome the scripted radio is using.
    #[must_use]
    pub fn outcome(&self) -> WifiOutcome {
        self.outcome
    }

    /// What the panel should show, or `None` when the panel belongs to the
    /// normal idle/stream path.
    #[must_use]
    pub fn screen(&self, now_us: u64) -> Option<Screen<'_>> {
        self.p.screen(ms(now_us))
    }

    /// The station's own join state: **the link**, and never the sticky
    /// result of the last credentials attempt.
    ///
    /// `docs/design/device-web.md`, the card 223 paragraph: in
    /// `GET /api/v1/status`, `wifi_state` means the link
    /// (`connected` / `connecting` / `disconnected`); the sticky `failed` of a
    /// posted attempt belongs to `GET /api/v1/wifi`, which is what
    /// [`Self::wifi_reply`] puts there. Card 228's HTTP conformance rule 8
    /// found the simulator reporting the sticky value in both places, exactly
    /// as firmware 0.4.0 does; this is the one derivation both callers now
    /// share, so they cannot drift apart again.
    ///
    /// In `Portal` the machine's own byte is used: there is no link to
    /// describe, and that is also what `GET_WIFI` (spec 6.3) answers - which
    /// this method deliberately does **not** change.
    pub(crate) fn link_state(&self) -> screeny_device_api::WifiState {
        use screeny_device_api::WifiState;
        match self.p.state() {
            State::Boot => WifiState::Disconnected,
            State::Joining | State::Trial => WifiState::Connecting,
            State::Online => WifiState::Connected,
            State::Portal => {
                WifiState::from_u8(self.p.wifi_state()).unwrap_or(WifiState::Disconnected)
            }
        }
    }

    /// `GET /api/v1/wifi`'s body, built from the machine and nothing else.
    ///
    /// A trial in flight or just finished is what the page that posted reads,
    /// so it wins; otherwise the station's own state is reported. **The
    /// machine decides which**, through
    /// [`Provisioner::trial_is_current`](screeny_provision::Provisioner::trial_is_current) -
    /// card 232, so that a LAN-side trial that failed keeps saying so after
    /// the previous network has come back, and so that the firmware and the
    /// simulator cannot answer this differently.
    #[must_use]
    pub fn wifi_reply(&self) -> WifiReply {
        if self.p.trial_is_current() {
            if let Some(t) = self.p.trial() {
                return WifiReply::from(t);
            }
        }
        WifiReply {
            state: self.link_state(),
            ssid: screeny_device_api::text::text(self.ssid()).filter(|s: &_| !s.is_empty()),
            ip: self.p.ip().map(screeny_device_api::text::ipv4_text),
            reason: None,
        }
    }

    // --- driving it --------------------------------------------------------

    /// Change the outcome the scripted radio uses for the *next* attempt.
    pub fn set_outcome(&mut self, o: WifiOutcome) {
        self.outcome = o;
    }

    /// Take the link down (spec section 7.3) or bring it back.
    pub fn set_link_down(&mut self, down: bool, now_us: u64, out: &mut Vec<Event>) {
        if down == self.link_down {
            return;
        }
        self.link_down = down;
        if down {
            out.push(Event::LinkDown);
            self.step(ProvEvent::LinkDown, ms(now_us), out);
        } else {
            out.push(Event::LinkUp);
            self.step(ProvEvent::LinkUp, ms(now_us), out);
        }
    }

    /// Somebody posted credentials: `POST /api/v1/wifi`, or `SET_WIFI`.
    ///
    /// The PSK is not a parameter. The caller has already dropped it.
    pub fn post_credentials(&mut self, ssid: &str, now_us: u64, out: &mut Vec<Event>) -> Posted {
        let before = self.p.state();
        self.step(ProvEvent::CredentialsPosted { ssid }, ms(now_us), out);
        if self.p.state() == State::Trial {
            Posted::Trial
        } else {
            Posted::Ignored(before)
        }
    }

    /// A station joined or left the soft-AP, which is what gates the portal's
    /// ten-minute retry.
    pub fn set_ap_client(&mut self, present: bool, now_us: u64, out: &mut Vec<Event>) {
        let ev = if present {
            ProvEvent::ApClientAssociated
        } else {
            ProvEvent::ApClientLeft
        };
        self.step(ev, ms(now_us), out);
    }

    /// The button's five-second hold: forget the network and raise the portal.
    pub fn wipe(&mut self, now_us: u64, out: &mut Vec<Event>) {
        self.step(ProvEvent::ButtonWipe, ms(now_us), out);
    }

    /// Advance the machine's timers and deliver whatever the scripted radio
    /// owes the attempt in flight.
    pub fn tick(&mut self, now_us: u64, out: &mut Vec<Event>) {
        let now = ms(now_us);
        if let Some(due) = self.pending {
            if now.wrapping_sub(due) < u32::MAX / 2 {
                self.pending = None;
                match self.outcome {
                    WifiOutcome::Ok => {
                        self.step(ProvEvent::Joined { ip: self.ip.octets() }, now, out);
                    }
                    WifiOutcome::Fail => {
                        self.step(
                            ProvEvent::JoinFailed {
                                reason: WifiFailReason::AuthError,
                            },
                            now,
                            out,
                        );
                    }
                    // Nothing comes back at all: the machine's own
                    // `join_attempt_ms` timeout is what ends this attempt.
                    WifiOutcome::Slow => {}
                }
            }
        }
        self.step(ProvEvent::Tick, now, out);
    }

    /// One step of the machine, with its actions carried out.
    fn step(&mut self, ev: ProvEvent<'_>, now_ms: u32, out: &mut Vec<Event>) {
        let before = self.p.state();
        let actions = self.p.step(ev, now_ms);
        for a in &actions {
            self.carry_out(*a, now_ms);
            out.push(Event::WifiAction { action: *a });
        }
        let after = self.p.state();
        if after != before {
            out.push(Event::WifiPhaseChanged {
                from: before,
                to: after,
            });
        }
    }

    fn carry_out(&mut self, a: Action, now_ms: u32) {
        match a {
            Action::StartJoin { .. } => {
                // `Slow` still arms the timer so that `StopJoin` has something
                // to cancel; `tick` simply never delivers anything for it.
                self.pending = Some(now_ms.wrapping_add(self.join_ms));
            }
            Action::StopJoin => self.pending = None,
            Action::CommitCredentials { which } => {
                let ssid = match which {
                    JoinTarget::Trial => self.p.trial().map(|t| t.ssid.as_str().to_string()),
                    JoinTarget::Stored => self.stored_ssid.clone(),
                };
                if let Some(ssid) = ssid {
                    self.stored_ssid = Some(ssid);
                }
                self.commits += 1;
            }
            Action::ClearCredentials => {
                self.stored_ssid = None;
                self.commits += 1;
            }
            // The simulator's soft-AP, mDNS and LAN server are not raised and
            // dropped: the AP side cannot be simulated honestly on a host
            // (card 224's "out of scope"), and the HTTP server and the mDNS
            // advertisement are up for the process's whole life. The action is
            // reported so a test can assert the machine asked for it.
            Action::RaiseAp | Action::DropAp | Action::Announce => {}
            _ => {}
        }
    }
}

/// Microseconds to the free-running millisecond counter the machine takes.
fn ms(now_us: u64) -> u32 {
    (now_us / 1_000) as u32
}
