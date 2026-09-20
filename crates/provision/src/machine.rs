//! The join/portal state machine of research 007 section 5.2, with no radio,
//! no clock and no I/O in it.
//!
//! Events in, [`Action`]s out, the caller passes `now_ms`. That is the same
//! split `crates/receiver` made for the receive rules, and for the same
//! reason: the whole of "three attempts, then the portal, and retry at 3 a.m.
//! if nobody is on it" becomes a unit test that runs in microseconds instead
//! of an hour on a bench.
//!
//! # The PSK is not here
//!
//! The machine never sees a password. [`Event::CredentialsPosted`] carries
//! only the SSID; the caller holds the posted credential itself and writes it
//! when the machine answers [`Action::CommitCredentials`]. So the invariant
//! of spec section 8.4 - the PSK never appears in a reply, a log line or on
//! the panel - is structural here rather than a rule someone has to remember:
//! there is no field for it to leak out of.
//!
//! # Driving it
//!
//! ```text
//! let mut p = Provisioner::new(&Config { ap_ssid, has_stored, .. });
//! for action in p.step(Event::Boot, now_ms()) { carry_out(action) }
//! // ... then, forever:
//! for action in p.step(Event::Tick, now_ms()) { carry_out(action) }
//! ```
//!
//! `Tick` may be as coarse as the caller likes - once a second is plenty -
//! but every timeout in here is measured from a stored instant rather than
//! counted in ticks, so a late tick delays a transition and never loses it.

use heapless::{String, Vec};
use screeny_proto::control::{state as tstate, wifi_state};

use crate::screen::{Layout, Screen};
use crate::uri::{self, UriForm};

/// Longest SSID this crate stores, which is spec section 6.3's `SET_WIFI`
/// limit. The *AP's* own name is much shorter (14 bytes, see [`crate::uri`]);
/// this is for the home network a trial join is aimed at.
pub const SSID_MAX: usize = screeny_proto::control::MAX_SSID_LEN;

/// Most actions one [`Provisioner::step`] can produce.
pub const MAX_ACTIONS: usize = 4;

/// What one step of the machine asks the caller to do.
pub type Actions = Vec<Action, MAX_ACTIONS>;

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// The timing constants of research 007 section 5.2, overridable so a test
/// does not have to wait ten real minutes to watch the portal retry.
///
/// [`Timing::SPEC`] is a device's behaviour and is the default. Anything else
/// is a test fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// How long one join attempt may take before it is counted as failed.
    /// Three of these is 007's "~45 s".
    pub join_attempt_ms: u32,
    /// Attempts per credential before moving on. 007 section 5.2: 3.
    pub join_attempts: u8,
    /// Attempts a *trial* join makes for a transient failure. An
    /// [`FailReason::AuthError`] is not retried at all - see
    /// [`Provisioner::step`].
    pub trial_attempts: u8,
    /// How long the soft-AP is held up after a trial join succeeds, so the
    /// phone standing on the portal can read the new address. ESP-IDF's
    /// `CONFIG_WIFI_PROV_AUTOSTOP_TIMEOUT` default, and it matches what the
    /// panel is showing.
    pub ap_grace_ms: u32,
    /// How often the portal retries the stored credentials while no client
    /// is associated to the AP. 007 section 5.2 argues this number: WLED
    /// throttles at 5 minutes, Tasmota's manager window is 3, and each retry
    /// costs a phone on the portal a ~45 s outage - hence 10 minutes, gated
    /// on nobody being there to notice.
    pub portal_retry_ms: u32,
    /// How long the link may be down in `Online` before the device goes back
    /// to `Joining`.
    pub link_down_ms: u32,
    /// How long the acquired address stays on the panel after a successful
    /// trial. The panel is the only channel that survives the radio changing
    /// channel, and Chrome on Android will not resolve `.local`.
    pub connected_screen_ms: u32,
    /// How long each portal layout is shown before the other one.
    pub screen_alternate_ms: u32,
}

impl Timing {
    /// The constants exactly as research 007 section 5.2 gives them.
    pub const SPEC: Timing = Timing {
        join_attempt_ms: 15_000,
        join_attempts: 3,
        trial_attempts: 3,
        ap_grace_ms: 30_000,
        portal_retry_ms: 600_000,
        link_down_ms: 60_000,
        connected_screen_ms: 60_000,
        screen_alternate_ms: 4_000,
    };
}

impl Default for Timing {
    fn default() -> Self {
        Timing::SPEC
    }
}

// 3 attempts x 15 s is the "~45 s" of 007 section 5.2.
const _: () = assert!(Timing::SPEC.join_attempt_ms * Timing::SPEC.join_attempts as u32 == 45_000);

/// How long the acquired address stays on the panel, for documentation links.
pub const CONNECTED_SCREEN_MS: u32 = Timing::SPEC.connected_screen_ms;

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// Where the device is in provisioning. Research 007 section 5.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Powered on, nothing decided yet.
    Boot,
    /// Trying stored or compile-time credentials. The AP may or may not be
    /// up: a retry out of `Portal` keeps it up.
    Joining,
    /// Associated and addressed. The LAN server and mDNS are up.
    Online,
    /// The captive portal is serving. Never terminal.
    Portal,
    /// Credentials were posted to the portal and are being tried. **Nothing
    /// is committed** and the AP stays up.
    Trial,
}

impl State {
    /// A name for a log line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            State::Boot => "boot",
            State::Joining => "joining",
            State::Online => "online",
            State::Portal => "portal",
            State::Trial => "trial",
        }
    }
}

/// Which credentials a join is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinTarget {
    /// What the settings store holds.
    Stored,
    /// The compile-time credentials, when the build has them. Spec section
    /// 8.3's step 2: tried after the stored ones fail and before the portal
    /// is raised, which is what makes a bench flash come straight up.
    Builtin,
    /// What was just posted to the portal, held by the caller.
    Trial,
}

/// Why a join attempt failed, in the vocabulary ESP-IDF's `wifi_provisioning`
/// uses and the portal page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    /// Wrong password: a four-way-handshake timeout, a MIC failure or an
    /// 802.1X rejection.
    AuthError,
    /// No AP with that SSID was found.
    NetworkNotFound,
    /// Anything else, including this crate's own attempt timeout.
    Other,
}

impl FailReason {
    /// The `reason` string of `GET /api/v1/wifi`, research 007 section 7.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FailReason::AuthError => "auth",
            FailReason::NetworkNotFound => "not_found",
            FailReason::Other => "other",
        }
    }
}

/// What happened to the credentials the portal last posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialOutcome {
    /// Still trying. The page reloads itself and asks again.
    Trying,
    /// Joined. The store has been written and the AP is on its grace timer.
    Connected,
    /// Failed, and **nothing was written to the store**.
    Failed(FailReason),
}

/// Where a trial join was posted from.
///
/// Spec section 8.2 says `SET_WIFI` works from any state, and card 223's LAN
/// settings page posts credentials to a device that is already on the network.
/// Those two arrivals want different behaviour on failure, a different panel
/// and a different telemetry overlay, and the only thing that tells them apart
/// is where the post came from - so the machine remembers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialOrigin {
    /// Posted to the captive portal, with the soft-AP up and, in all
    /// likelihood, a phone standing on it waiting to be told how it went. The
    /// AP stays up through the trial; a failure goes back to [`State::Portal`]
    /// so the same page can say why.
    Portal,
    /// Posted while the device was already [`State::Online`] or
    /// [`State::Joining`] - the LAN settings page, or a `SET_WIFI` datagram.
    /// **No AP is raised**: the device has one station, so it must drop the
    /// association it has to try the new network, and there is nobody on a
    /// setup network to keep informed. A failure goes back to the *previous*
    /// network rather than to the portal.
    Online,
}

/// The last trial, for the portal page's full-page reload to read.
///
/// No password field, by construction: see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trial {
    /// What the trial is or was trying to join.
    pub ssid: String<SSID_MAX>,
    /// How it went.
    pub outcome: TrialOutcome,
    /// The address acquired, once it succeeded.
    pub ip: Option<[u8; 4]>,
    /// Where the credentials were posted from, which decides what a failure
    /// falls back to. See [`TrialOrigin`].
    pub origin: TrialOrigin,
}

/// Something that happened to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event<'a> {
    /// The device finished booting and the store has been read.
    Boot,
    /// The radio associated and DHCP (or the static config) gave us `ip`.
    Joined {
        /// The address acquired.
        ip: [u8; 4],
    },
    /// The join attempt in flight failed.
    JoinFailed {
        /// What the radio said.
        reason: FailReason,
    },
    /// `POST /api/v1/wifi` arrived. The caller is holding the credential;
    /// this event carries only the SSID.
    CredentialsPosted {
        /// The network the user typed.
        ssid: &'a str,
    },
    /// A station associated to the soft-AP, so somebody is on the portal.
    ApClientAssociated,
    /// A station left the soft-AP.
    ApClientLeft,
    /// The `Online` link went away.
    LinkDown,
    /// The `Online` link came back before [`Timing::link_down_ms`] expired.
    LinkUp,
    /// The button's five-second hold (card 202/231): forget the network.
    ButtonWipe,
    /// Time passed. Every timeout in the machine is checked here.
    Tick,
}

/// What the caller should do. The machine decides *what*; the firmware and
/// the simulator each decide *how*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Begin a join with these credentials. `attempt` counts from 1 and is
    /// there for the log line, not for the radio.
    StartJoin {
        /// Which credentials to use.
        which: JoinTarget,
        /// 1-based attempt number within this target.
        attempt: u8,
    },
    /// Abandon the join in flight, if any.
    StopJoin,
    /// Bring up the soft-AP (APSTA), DHCP, the DNS catch-all and the portal
    /// HTTP server.
    RaiseAp,
    /// Take the soft-AP and its services down.
    DropAp,
    /// Write these credentials to the settings store. This is the **only**
    /// thing that commits, and it never happens before a join succeeded.
    CommitCredentials {
        /// Which credentials became the stored ones.
        which: JoinTarget,
    },
    /// Erase the stored credentials.
    ClearCredentials,
    /// We are on the LAN: start mDNS and the LAN HTTP server.
    Announce,
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// What the machine needs to know before it can decide anything.
#[derive(Debug, Clone, Copy)]
pub struct Config<'a> {
    /// The soft-AP's name, always `screeny-<id>` (research 007 section 9.1).
    pub ap_ssid: &'a str,
    /// Whether the settings store holds credentials.
    pub has_stored: bool,
    /// Whether this build was compiled with credentials (device-web decision
    /// 6: when present they seed an empty store; a build without them boots
    /// straight to the portal, which is what a public repo needs).
    pub has_builtin: bool,
    /// Which `WIFI:` spelling the QR carries.
    pub form: UriForm,
    /// Timing. [`Timing::SPEC`] unless this is a test.
    pub timing: Timing,
}

impl Default for Config<'_> {
    fn default() -> Self {
        Config {
            ap_ssid: "screeny-000000",
            has_stored: false,
            has_builtin: false,
            form: UriForm::NoPass,
            timing: Timing::SPEC,
        }
    }
}

/// The join/portal state machine.
///
/// `Debug` is safe to log: there is no credential in it.
#[derive(Debug, Clone)]
pub struct Provisioner {
    ap_ssid: String<SSID_MAX>,
    timing: Timing,
    form: UriForm,

    state: State,
    has_stored: bool,
    has_builtin: bool,

    /// Which credentials the join or trial in flight is using.
    target: JoinTarget,
    /// 1-based attempt within `target`.
    attempt: u8,
    /// When the attempt in flight started.
    attempt_since: u32,

    ap_up: bool,
    ap_clients: u8,
    /// When the AP's post-success grace started, if it is running.
    ap_grace_since: Option<u32>,

    /// When the current `Portal` episode began, for the retry timer.
    portal_since: u32,
    /// Whether the portal is up because something failed, which is the
    /// difference between `GET_WIFI` reading `FAILED` and `DISCONNECTED`.
    portal_after_failure: bool,

    /// When the link went down in `Online`.
    link_down_since: Option<u32>,

    trial: Option<Trial>,
    /// A [`TrialOrigin::Online`] trial failed and we went back to the previous
    /// network. Sticky until the next post or a reboot, because otherwise the
    /// old network reconnecting a few seconds later reads as "your new network
    /// worked". Firmware 0.3.0's `SET_WIFI` does the same.
    trial_failed_sticky: bool,
    ip: Option<[u8; 4]>,
    /// When a successful *trial* put the address on the panel.
    connected_since: Option<u32>,
}

impl Provisioner {
    /// A machine in [`State::Boot`]. Nothing happens until it is given
    /// [`Event::Boot`].
    #[must_use]
    pub fn new(cfg: &Config<'_>) -> Self {
        let mut ap_ssid: String<SSID_MAX> = String::new();
        let _ = ap_ssid.push_str(crate::screen::cut(cfg.ap_ssid, SSID_MAX));
        Provisioner {
            ap_ssid,
            timing: cfg.timing,
            form: cfg.form,
            state: State::Boot,
            has_stored: cfg.has_stored,
            has_builtin: cfg.has_builtin,
            target: JoinTarget::Stored,
            attempt: 0,
            attempt_since: 0,
            ap_up: false,
            ap_clients: 0,
            ap_grace_since: None,
            portal_since: 0,
            portal_after_failure: false,
            link_down_since: None,
            trial: None,
            trial_failed_sticky: false,
            ip: None,
            connected_since: None,
        }
    }

    // --- queries -----------------------------------------------------------

    /// Where the machine is.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// The telemetry `state` byte overlay of spec section 7.3: `PROVISIONING`
    /// while the portal is up, and `None` the rest of the time, meaning the
    /// stream state machine's own byte stands.
    ///
    /// It is an *overlay*: frame handling continues underneath, so a sender
    /// on the LAN that is still streaming keeps streaming while the device is
    /// in portal mode on the AP side.
    /// A [`TrialOrigin::Online`] trial is not provisioning: no AP was raised,
    /// frames may still be arriving around the rejoin and nothing about the
    /// panel is "in setup", so it gets no overlay.
    #[must_use]
    pub fn overlay_state(&self) -> Option<u8> {
        match self.state {
            State::Portal => Some(tstate::PROVISIONING),
            State::Trial if self.trial_origin() == Some(TrialOrigin::Portal) => {
                Some(tstate::PROVISIONING)
            }
            _ => None,
        }
    }

    /// The trailing byte of a `GET_WIFI` reply, research 007 section 5.2.
    ///
    /// A [`TrialOrigin::Online`] trial that failed reads `FAILED` until the
    /// next post or a reboot, *even once the previous network is back*: the
    /// question the byte answers is "did the credentials you just gave me
    /// work", and the old association coming back is not an answer to it.
    #[must_use]
    pub fn wifi_state(&self) -> u8 {
        if self.trial_failed_sticky {
            return wifi_state::FAILED;
        }
        match self.state {
            State::Boot => wifi_state::DISCONNECTED,
            State::Joining | State::Trial => wifi_state::CONNECTING,
            State::Online => wifi_state::CONNECTED,
            State::Portal => {
                if self.portal_after_failure {
                    wifi_state::FAILED
                } else {
                    wifi_state::DISCONNECTED
                }
            }
        }
    }

    /// The soft-AP's name.
    #[must_use]
    pub fn ap_ssid(&self) -> &str {
        &self.ap_ssid
    }

    /// Whether the soft-AP is up right now.
    #[must_use]
    pub fn ap_up(&self) -> bool {
        self.ap_up
    }

    /// How many stations are associated to the soft-AP.
    #[must_use]
    pub fn ap_clients(&self) -> u8 {
        self.ap_clients
    }

    /// Whether the store holds credentials, as far as the machine knows.
    #[must_use]
    pub fn has_stored(&self) -> bool {
        self.has_stored
    }

    /// The address we hold, if any.
    #[must_use]
    pub fn ip(&self) -> Option<[u8; 4]> {
        self.ip
    }

    /// The last trial, for `GET /api/v1/wifi` and the portal page's
    /// full-page reload.
    #[must_use]
    pub fn trial(&self) -> Option<&Trial> {
        self.trial.as_ref()
    }

    /// Where the last trial was posted from, if there was one.
    #[must_use]
    pub fn trial_origin(&self) -> Option<TrialOrigin> {
        self.trial.as_ref().map(|t| t.origin)
    }

    /// Whether [`trial`](Self::trial) is still what `GET /api/v1/wifi` should
    /// report, rather than the station's own state.
    ///
    /// True while a trial is running or the portal is showing its result, and
    /// true for a failed [`TrialOrigin::Online`] trial for as long as
    /// [`wifi_state`](Self::wifi_state) reads `FAILED` - so the page that
    /// posted the credentials keeps being told they did not work, whatever the
    /// station reconnected to in the meantime.
    #[must_use]
    pub fn trial_is_current(&self) -> bool {
        self.trial.is_some()
            && (matches!(self.state, State::Trial | State::Portal) || self.trial_failed_sticky)
    }

    /// What the panel should show, or `None` when the panel belongs to the
    /// normal idle/stream path.
    ///
    /// The two portal layouts alternate every
    /// [`Timing::screen_alternate_ms`]; the caller drives that by passing the
    /// clock, exactly as it drives every other timeout here. A name no
    /// version 2-L code can carry never gets [`Layout::QrAndName`], so
    /// [`crate::render`] cannot fail on what this returns.
    #[must_use]
    pub fn screen(&self, now_ms: u32) -> Option<Screen<'_>> {
        match self.state {
            // A `TrialOrigin::Online` trial never raised an AP, so there is no
            // portal to point anybody at: the panel stays the stream's.
            State::Trial if self.trial_origin() == Some(TrialOrigin::Online) => None,
            State::Portal | State::Trial => {
                let alternate = (now_ms / self.timing.screen_alternate_ms) % 2 == 1;
                let layout = if alternate || !uri::fits(self.form, &self.ap_ssid) {
                    Layout::Text
                } else {
                    Layout::QrAndName
                };
                Some(Screen::Portal {
                    ssid: &self.ap_ssid,
                    layout,
                    form: self.form,
                })
            }
            State::Online => match (self.connected_since, self.ip) {
                (Some(since), Some(ip))
                    if now_ms.wrapping_sub(since) < self.timing.connected_screen_ms =>
                {
                    Some(Screen::Connected { ip })
                }
                _ => None,
            },
            _ => None,
        }
    }

    // --- the step ----------------------------------------------------------

    /// Feed the machine one event and the current time.
    ///
    /// `now_ms` is a free-running millisecond counter and is allowed to wrap
    /// (a `u32` of milliseconds wraps every 49.7 days, and this device is
    /// meant to run unattended for months). Every comparison in here is
    /// wrapping, so a wrap costs at most one late transition and never a lost
    /// one.
    ///
    /// The returned actions are in the order they should be carried out.
    pub fn step(&mut self, ev: Event<'_>, now_ms: u32) -> Actions {
        let mut out = Actions::new();

        // These two are bookkeeping in every state, so they are handled once.
        match ev {
            Event::ApClientAssociated => {
                self.ap_clients = self.ap_clients.saturating_add(1);
                return out;
            }
            Event::ApClientLeft => {
                self.ap_clients = self.ap_clients.saturating_sub(1);
                return out;
            }
            // "any state -> PORTAL", research 007 section 5.2's last row.
            Event::ButtonWipe => {
                if matches!(self.state, State::Joining | State::Trial) {
                    push(&mut out, Action::StopJoin);
                }
                push(&mut out, Action::ClearCredentials);
                self.has_stored = false;
                self.trial = None;
                self.trial_failed_sticky = false;
                self.ip = None;
                self.connected_since = None;
                self.link_down_since = None;
                self.enter_portal(now_ms, false, &mut out);
                return out;
            }
            _ => {}
        }

        match self.state {
            State::Boot => {
                // `Event::CredentialsPosted` cannot reach `Boot`: nothing is
                // serving yet - no portal, no LAN server, no control socket -
                // until `Event::Boot` has been processed and the machine has
                // left this state. If one arrives anyway it is a caller bug,
                // and dropping it is the right answer: there is no radio to
                // give it to and nothing committed that a trial could fall
                // back to.
                if matches!(ev, Event::Boot) {
                    self.begin(now_ms, &mut out);
                }
            }

            State::Joining => match ev {
                Event::Joined { ip } => self.go_online(now_ms, ip, &mut out),
                Event::JoinFailed { reason } => self.attempt_failed(now_ms, reason, &mut out),
                Event::Tick if self.attempt_expired(now_ms) => {
                    self.attempt_failed(now_ms, FailReason::Other, &mut out);
                }
                // Spec 8.2: `SET_WIFI` works from any state. The attempt in
                // flight is abandoned - there is one station - and the new
                // credentials are tried instead, without raising the AP.
                Event::CredentialsPosted { ssid } => {
                    push(&mut out, Action::StopJoin);
                    self.begin_trial(now_ms, ssid, TrialOrigin::Online, &mut out);
                }
                _ => {}
            },

            State::Trial => match ev {
                Event::Joined { ip } => self.go_online(now_ms, ip, &mut out),
                Event::JoinFailed { reason } => self.attempt_failed(now_ms, reason, &mut out),
                Event::Tick if self.attempt_expired(now_ms) => {
                    self.attempt_failed(now_ms, FailReason::Other, &mut out);
                }
                // A second POST while the first is still being tried: the
                // user corrected a typo. Start over with the new one, from
                // wherever the trial it replaces came from - the second post
                // arrived down the same channel as the first.
                Event::CredentialsPosted { ssid } => {
                    let origin = self.trial_origin().unwrap_or(TrialOrigin::Portal);
                    push(&mut out, Action::StopJoin);
                    self.begin_trial(now_ms, ssid, origin, &mut out);
                }
                _ => {}
            },

            State::Portal => match ev {
                Event::CredentialsPosted { ssid } => {
                    self.begin_trial(now_ms, ssid, TrialOrigin::Portal, &mut out);
                }
                // The 3 a.m. router reboot heals itself - but only while
                // nobody is standing on the portal, because a retry costs
                // them a ~45 s outage.
                Event::Tick
                    if self.has_stored
                        && self.ap_clients == 0
                        && now_ms.wrapping_sub(self.portal_since)
                            >= self.timing.portal_retry_ms =>
                {
                    self.start_join(JoinTarget::Stored, now_ms, &mut out);
                }
                _ => {}
            },

            State::Online => match ev {
                // Spec 8.2 again, and card 223's LAN settings page. The AP is
                // *not* raised: see [`TrialOrigin::Online`].
                Event::CredentialsPosted { ssid } => {
                    self.begin_trial(now_ms, ssid, TrialOrigin::Online, &mut out);
                }
                Event::LinkDown => {
                    if self.link_down_since.is_none() {
                        self.link_down_since = Some(now_ms);
                    }
                }
                Event::LinkUp => self.link_down_since = None,
                Event::Tick => {
                    if let Some(since) = self.ap_grace_since {
                        if now_ms.wrapping_sub(since) >= self.timing.ap_grace_ms {
                            self.ap_grace_since = None;
                            self.ap_up = false;
                            self.ap_clients = 0;
                            push(&mut out, Action::DropAp);
                        }
                    }
                    if let Some(since) = self.link_down_since {
                        if now_ms.wrapping_sub(since) >= self.timing.link_down_ms {
                            self.link_down_since = None;
                            self.ip = None;
                            self.connected_since = None;
                            self.start_join(JoinTarget::Stored, now_ms, &mut out);
                        }
                    }
                }
                _ => {}
            },
        }
        out
    }

    // --- transitions -------------------------------------------------------

    /// `BOOT`: stored credentials first, then the compile-time ones, then the
    /// portal.
    ///
    /// Research 007's table says "store empty -> PORTAL" and separately that
    /// the compile-time credentials are step 2 of spec 8.3. Device-web
    /// decision 6 settles the overlap: a build *with* compile-time
    /// credentials and an empty store tries them, and on success
    /// [`Action::CommitCredentials`] seeds the store with them.
    fn begin(&mut self, now_ms: u32, out: &mut Actions) {
        if self.has_stored {
            self.start_join(JoinTarget::Stored, now_ms, out);
        } else if self.has_builtin {
            self.start_join(JoinTarget::Builtin, now_ms, out);
        } else {
            self.enter_portal(now_ms, false, out);
        }
    }

    fn start_join(&mut self, which: JoinTarget, now_ms: u32, out: &mut Actions) {
        self.state = State::Joining;
        self.target = which;
        self.attempt = 1;
        self.attempt_since = now_ms;
        push(
            out,
            Action::StartJoin {
                which,
                attempt: self.attempt,
            },
        );
    }

    fn begin_trial(&mut self, now_ms: u32, ssid: &str, origin: TrialOrigin, out: &mut Actions) {
        let mut name: String<SSID_MAX> = String::new();
        let _ = name.push_str(crate::screen::cut(ssid, SSID_MAX));
        self.trial = Some(Trial {
            ssid: name,
            outcome: TrialOutcome::Trying,
            ip: None,
            origin,
        });
        // A new post is a new question, so the last one's sticky answer goes.
        self.trial_failed_sticky = false;
        self.state = State::Trial;
        self.target = JoinTarget::Trial;
        self.attempt = 1;
        self.attempt_since = now_ms;
        if origin == TrialOrigin::Online {
            // The station is being taken away from whatever it was on, so the
            // address we were reporting is no longer ours. Nothing is raised
            // in its place: there is no AP and no portal on this path.
            self.ip = None;
            self.connected_since = None;
            self.link_down_since = None;
        }
        // On the portal path the AP is *not* dropped: that is the whole point
        // of APSTA here, and it is what lets the page report the result.
        push(
            out,
            Action::StartJoin {
                which: JoinTarget::Trial,
                attempt: 1,
            },
        );
    }

    fn attempt_expired(&self, now_ms: u32) -> bool {
        now_ms.wrapping_sub(self.attempt_since) >= self.timing.join_attempt_ms
    }

    /// One attempt failed. Retry, move to the next credential, or give up.
    fn attempt_failed(&mut self, now_ms: u32, reason: FailReason, out: &mut Actions) {
        if self.state == State::Trial {
            // A wrong password is deterministic: retrying it three times only
            // makes the person holding the phone wait 45 s for the same
            // answer. 007's diagram says "3 tries" without distinguishing;
            // this is card 221's reading of it.
            let more = reason != FailReason::AuthError && self.attempt < self.timing.trial_attempts;
            if more {
                self.attempt += 1;
                self.attempt_since = now_ms;
                push(
                    out,
                    Action::StartJoin {
                        which: JoinTarget::Trial,
                        attempt: self.attempt,
                    },
                );
                return;
            }
            // **Nothing is written to the store** on either path.
            let origin = self.trial_origin().unwrap_or(TrialOrigin::Portal);
            if let Some(t) = self.trial.as_mut() {
                t.outcome = TrialOutcome::Failed(reason);
                t.ip = None;
            }
            match origin {
                // The AP never went down, so the page that posted is still
                // there to be told why.
                TrialOrigin::Portal => self.enter_portal(now_ms, true, out),
                // Nobody is on a setup network and the store still holds the
                // network that was working a moment ago: go back to it rather
                // than stranding the panel on an AP nobody is looking for.
                // The failure stays readable until the next post or a reboot.
                TrialOrigin::Online => {
                    self.trial_failed_sticky = true;
                    if self.has_stored {
                        self.start_join(JoinTarget::Stored, now_ms, out);
                    } else if self.has_builtin {
                        // Reachable only from `Joining` on a build with
                        // compile-time credentials and an empty store: there
                        // is no previous network to go back to, so step 2 of
                        // spec 8.3 is the next best thing.
                        self.start_join(JoinTarget::Builtin, now_ms, out);
                    } else {
                        // Nothing to fall back to at all. The portal is the
                        // only way back in.
                        self.enter_portal(now_ms, true, out);
                    }
                }
            }
            return;
        }

        // State::Joining.
        if self.attempt < self.timing.join_attempts {
            self.attempt += 1;
            self.attempt_since = now_ms;
            push(
                out,
                Action::StartJoin {
                    which: self.target,
                    attempt: self.attempt,
                },
            );
            return;
        }
        if self.target == JoinTarget::Stored && self.has_builtin {
            self.start_join(JoinTarget::Builtin, now_ms, out);
            return;
        }
        self.enter_portal(now_ms, true, out);
    }

    fn go_online(&mut self, now_ms: u32, ip: [u8; 4], out: &mut Actions) {
        let from_trial = self.state == State::Trial;
        // The only commit in the whole machine, and it happens *after* a join
        // succeeded, never before.
        match self.target {
            JoinTarget::Trial => {
                push(
                    out,
                    Action::CommitCredentials {
                        which: JoinTarget::Trial,
                    },
                );
                self.has_stored = true;
            }
            // Device-web decision 6: compile-time credentials seed an empty
            // store, so a bench flash comes up once and is provisioned after.
            JoinTarget::Builtin => {
                push(
                    out,
                    Action::CommitCredentials {
                        which: JoinTarget::Builtin,
                    },
                );
                self.has_stored = true;
            }
            JoinTarget::Stored => {}
        }
        let origin = self.trial_origin();
        if from_trial {
            if let Some(t) = self.trial.as_mut() {
                t.outcome = TrialOutcome::Connected;
                t.ip = Some(ip);
            }
        }
        // The address goes on the panel only for a portal trial, where the
        // panel is the one channel that survives the radio changing channel
        // and the phone is about to lose the AP. A `TrialOrigin::Online` post
        // came from a browser on the LAN that is being told the address in
        // its own reply, so the panel stays the stream's.
        self.connected_since = if from_trial && origin == Some(TrialOrigin::Portal) {
            Some(now_ms)
        } else {
            None
        };
        self.state = State::Online;
        self.ip = Some(ip);
        self.link_down_since = None;
        self.portal_after_failure = false;
        // Hold the AP for the grace window whatever brought us here: on the
        // trial path so the page can report the new address, and on the
        // portal-retry path because dropping it instantly would be no kinder.
        self.ap_grace_since = if self.ap_up { Some(now_ms) } else { None };
        push(out, Action::Announce);
    }

    fn enter_portal(&mut self, now_ms: u32, after_failure: bool, out: &mut Actions) {
        self.state = State::Portal;
        self.portal_since = now_ms;
        self.portal_after_failure = after_failure;
        self.ap_grace_since = None;
        self.connected_since = None;
        if !self.ap_up {
            self.ap_up = true;
            self.ap_clients = 0;
            push(out, Action::RaiseAp);
        }
    }
}

/// `Vec::push` cannot fail here - [`MAX_ACTIONS`] is sized for the largest
/// step - but this crate does not panic on a device with no MMU, so a bug
/// would drop an action rather than reboot the panel. The debug assertion is
/// what makes a test notice.
fn push(out: &mut Actions, a: Action) {
    let ok = out.push(a).is_ok();
    debug_assert!(ok, "MAX_ACTIONS too small for {a:?}");
}
