//! Card 387: a panel that was online must come back to the LAN on its own,
//! within a bounded time of its network returning, whatever the access point
//! did in between.
//!
//! These run the machine at **`Timing::SPEC`** - the real 60 s, 15 s and 30 s
//! - against a small model of the radio and the access point, ticked once a
//! second exactly as `firmware/src/provision.rs` ticks it. The model is the
//! firmware's behaviour, not an idealised one:
//!
//! - a join finds no network in [`SCAN_MS`] when the access point is gone,
//!   and otherwise associates and then waits [`DHCP_MS`] for a lease (this
//!   mesh answers DHCP in 10-12 s, `provision.rs`'s `DHCP_WAIT` notes);
//! - the station does **not** reassociate by itself when the access point
//!   comes back: esp-radio has no auto-reconnect, so the only ways back are a
//!   join the machine asks for and the driver's re-association after `DropAp`;
//! - `DropAp` is a mode change, so it restarts the radio and the station has
//!   to associate (and get a lease) again before the LAN can reach it.
//!
//! "On the LAN" is what the owner's Studio needs: `Online`, the soft-AP down
//! (while it is up both HTTP workers serve only the setup network,
//! `http.rs`'s `http_task`), and the station associated with an address.

use screeny_provision::{Action, Config, Event, FailReason, JoinTarget, Provisioner, Screen, State, Timing, UriForm};

/// How long a join takes to say "no such network" when the access point is
/// gone: one all-channel scan.
const SCAN_MS: u32 = 3_000;
/// Association plus DHCP when the access point is there.
const DHCP_MS: u32 = 12_000;
/// The card's bound: back on the LAN within this long of the network being
/// back, once the machine has had to give up on it.
const BOUND_MS: u32 = 60_000;

const LAN: [u8; 4] = [192, 168, 7, 221];

/// The access point, the station and the machine, one second at a time.
struct World {
    p: Provisioner,
    now: u32,
    /// `[from, to)` during which the access point is gone.
    outage: (u32, u32),
    /// The address the next lease hands out.
    lease: [u8; 4],
    /// A join in flight: when it resolves.
    join_done_at: Option<u32>,
    /// The driver's post-`DropAp` re-association in flight.
    reassoc_done_at: Option<u32>,
    /// Station associated with an address.
    associated: bool,
    /// What the driver last told the machine about the link.
    link_was_up: bool,
    /// The panel's AP clients as the radio sees them, which the machine may
    /// or may not have heard about.
    log: Vec<(u32, Action)>,
}

impl World {
    /// A panel online on the stored network at t = 0.
    fn online() -> World {
        let mut p = Provisioner::new(&Config {
            ap_ssid: "screeny-4a00a4",
            has_stored: true,
            form: UriForm::NoPass,
            timing: Timing::SPEC,
        });
        let mut w = World {
            p: p.clone(),
            now: 0,
            outage: (u32::MAX, u32::MAX),
            lease: LAN,
            join_done_at: None,
            reassoc_done_at: None,
            associated: false,
            link_was_up: false,
            log: Vec::new(),
        };
        let a = p.step(Event::Boot, 0);
        w.p = p;
        w.carry_out(a.as_slice());
        // Run the boot join to completion.
        while w.p.state() != State::Online {
            w.second();
        }
        assert!(w.on_lan());
        w
    }

    fn ap_present(&self, t: u32) -> bool {
        !(self.outage.0..self.outage.1).contains(&t)
    }

    fn on_lan(&self) -> bool {
        self.p.state() == State::Online && !self.p.ap_up() && self.associated
    }

    fn step(&mut self, ev: Event<'_>) {
        let a = self.p.step(ev, self.now);
        self.carry_out(a.as_slice());
    }

    fn carry_out(&mut self, actions: &[Action]) {
        for &a in actions {
            self.log.push((self.now, a));
            match a {
                Action::StartJoin { .. } => {
                    // The driver disconnects before a join (card 212).
                    self.associated = false;
                    self.join_done_at = Some(self.now + SCAN_MS);
                }
                Action::StopJoin => self.join_done_at = None,
                // APSTA -> STA restarts the radio: the association goes, and
                // the driver re-associates (`Driver::drop_ap`).
                Action::DropAp => {
                    self.associated = false;
                    self.reassoc_done_at = Some(self.now + SCAN_MS);
                }
                // STA -> APSTA restarts the radio too.
                Action::RaiseAp => self.associated = false,
                _ => {}
            }
        }
    }

    /// One second: the access point, the radio, then the driver's tick.
    fn second(&mut self) {
        self.now += 1_000;
        let now = self.now;
        if !self.ap_present(now) {
            self.associated = false;
        }
        // The join the machine asked for. The driver runs it to completion
        // before it ticks again, so it is resolved here as one event.
        if let Some(at) = self.join_done_at {
            if now >= at {
                self.join_done_at = None;
                if self.ap_present(now) {
                    self.now += DHCP_MS;
                    self.associated = true;
                    self.link_was_up = true;
                    let ip = self.lease;
                    self.step(Event::Joined { ip });
                } else {
                    self.step(Event::JoinFailed { reason: FailReason::NetworkNotFound });
                }
                return;
            }
            // Mid-join: no tick (the driver is inside `run_join`).
            return;
        }
        if let Some(at) = self.reassoc_done_at {
            if now >= at {
                self.reassoc_done_at = None;
                if self.ap_present(now) {
                    self.now += DHCP_MS;
                    self.associated = true;
                }
            }
        }
        // The driver's link reconciliation, then `Tick`.
        let up = self.associated;
        if up != self.link_was_up {
            self.link_was_up = up;
            let ev = if up { Event::LinkUp } else { Event::LinkDown };
            self.step(ev);
        }
        self.step(Event::Tick);
    }

    /// Run until the panel is back on the LAN after the outage, or `limit`.
    /// Returns how long after the access point came back that was.
    fn back_after_outage(&mut self, limit_ms: u32) -> u32 {
        let back = self.outage.1;
        while self.now < back || !self.on_lan() {
            assert!(self.now < back + limit_ms, "still off the LAN {} s after the AP came back: {:?}, log {:?}",
                (self.now - back) / 1000, self.p.state(), self.log);
            self.second();
        }
        self.now - back
    }
}

fn outage_of(secs: u32) -> u32 {
    let mut w = World::online();
    let start = w.now + 5_000;
    w.outage = (start, start + secs * 1_000);
    w.back_after_outage(30 * 60_000)
}

/// A ten-second blip: the link is down, nothing reconnects the station by
/// itself, and the machine rejoins at `link_down_ms`. Never reaches the
/// portal.
#[test]
fn a_ten_second_outage_is_back_by_the_link_down_rule() {
    let mut w = World::online();
    w.outage = (w.now + 5_000, w.now + 15_000);
    let after = w.back_after_outage(10 * 60_000);
    assert!(!w.log.iter().any(|(_, a)| *a == Action::RaiseAp), "a blip must not raise the setup network");
    // Link down at the outage start; 60 s later a join; join + DHCP.
    assert!(after <= Timing::SPEC.link_down_ms + SCAN_MS + DHCP_MS + 2_000, "back {} s after", after / 1000);
}

/// A minute: the rejoin at `link_down_ms` lands just after it comes back.
#[test]
fn a_sixty_second_outage_comes_back_within_the_bound() {
    let after = outage_of(60);
    assert!(after <= BOUND_MS, "back {} s after the AP returned", after / 1000);
}

/// Five minutes: the three attempts fail, the portal goes up, and the portal's
/// retry is what brings it back. At 007's ten-minute retry this was up to
/// ~10 minutes after the network was back (card 387's finding A).
#[test]
fn a_five_minute_outage_comes_back_within_the_bound() {
    let mut w = World::online();
    let start = w.now + 5_000;
    w.outage = (start, start + 300_000);
    let after = w.back_after_outage(30 * 60_000);
    assert!(w.log.iter().any(|(_, a)| *a == Action::RaiseAp), "five minutes is long enough to open the portal");
    assert!(after <= BOUND_MS, "back {} s after the AP returned", after / 1000);
    assert!(!w.p.ap_up(), "the setup network is down again");
}

/// Every outage length from 1 s to 15 min, and every phase of the portal's
/// retry cycle the return can land in: never longer than the bound once the
/// portal is involved, and never longer than the link-down rule before it.
#[test]
fn no_outage_length_strands_the_panel() {
    let worst_blip = Timing::SPEC.link_down_ms + SCAN_MS + DHCP_MS + 2_000;
    for secs in (1..=900).step_by(7) {
        let after = outage_of(secs);
        let limit = if secs * 1_000 < Timing::SPEC.link_down_ms { worst_blip } else { BOUND_MS.max(worst_blip.saturating_sub(secs * 1_000)) };
        assert!(after <= limit, "a {secs} s outage: back {} s after the AP returned", after / 1000);
    }
}

/// The access point comes back handing out a different address (a router
/// replaced or reset in the outage). The panel takes the new one.
#[test]
fn an_address_change_during_the_outage_is_taken() {
    let mut w = World::online();
    let start = w.now + 5_000;
    w.outage = (start, start + 300_000);
    w.lease = [192, 168, 7, 54];
    w.back_after_outage(30 * 60_000);
    assert_eq!(w.p.ip(), Some([192, 168, 7, 54]));
}

/// Card 387's finding C: a phone that left while the radio loop was busy
/// leaves the machine believing it is still there. That used to suppress the
/// retry for ever; now the belief runs out after `portal_client_hold_ms`.
#[test]
fn a_stale_ap_client_count_does_not_strand_the_panel() {
    let mut w = World::online();
    let start = w.now + 5_000;
    w.outage = (start, start + 300_000);
    // Run into the portal, then a phone joins - and its leaving is never heard.
    while w.p.state() != State::Portal {
        w.second();
    }
    w.step(Event::ApClientAssociated);
    let joined_at = w.now;
    let after = w.back_after_outage(30 * 60_000);
    let back_at = w.outage.1 + after;
    assert!(
        back_at <= joined_at + Timing::SPEC.portal_client_hold_ms + Timing::SPEC.portal_retry_ms + BOUND_MS,
        "a phone nobody heard leave held the panel off the LAN until {} s",
        back_at / 1000
    );
}

/// A phone that is really on the portal still holds the retry off while it
/// keeps talking to the radio (each event refreshes the belief).
#[test]
fn a_phone_that_is_on_the_portal_still_holds_the_retry_off() {
    let mut p = Provisioner::new(&Config { has_stored: true, timing: Timing::SPEC, ..Config::default() });
    p.step(Event::Boot, 0);
    for t in [15_000, 30_000, 45_000] {
        p.step(Event::JoinFailed { reason: FailReason::NetworkNotFound }, t);
    }
    assert_eq!(p.state(), State::Portal);
    p.step(Event::ApClientAssociated, 50_000);
    let mut t = 50_000;
    while t < 50_000 + 2 * Timing::SPEC.portal_client_hold_ms {
        t += 1_000;
        assert!(p.step(Event::Tick, t).is_empty(), "retry at {} s with a phone on the portal", t / 1000);
        // The phone reconnects now and then (a second device, a rejoin).
        if t % 300_000 == 0 {
            p.step(Event::ApClientLeft, t);
            p.step(Event::ApClientAssociated, t);
        }
    }
    assert_eq!(p.state(), State::Portal);
}

/// Card 387's finding B: the association outlived a failed attempt and its
/// lease landed while the machine sat in `Portal`. The firmware now feeds
/// that as `Joined`, and the machine takes it - straight to the LAN, with
/// the setup network dropped because nobody is on it.
#[test]
fn a_late_lease_under_the_portal_takes_the_panel_back_to_the_lan() {
    let mut p = Provisioner::new(&Config { has_stored: true, timing: Timing::SPEC, ..Config::default() });
    p.step(Event::Boot, 0);
    for t in [15_000, 30_000, 45_000] {
        p.step(Event::JoinFailed { reason: FailReason::Other }, t);
    }
    assert_eq!(p.state(), State::Portal);
    let a = p.step(Event::Joined { ip: LAN }, 50_000);
    assert_eq!(p.state(), State::Online);
    assert_eq!(a.as_slice(), [Action::Announce, Action::DropAp]);
    assert_eq!(p.ip(), Some(LAN));
    assert!(!p.ap_up());
}

/// With a phone on the portal, the same late lease keeps the AP for its grace
/// window, as any join does with somebody on it.
#[test]
fn a_late_lease_with_a_phone_on_the_portal_keeps_the_grace_window() {
    let mut p = Provisioner::new(&Config { has_stored: true, timing: Timing::SPEC, ..Config::default() });
    p.step(Event::Boot, 0);
    for t in [15_000, 30_000, 45_000] {
        p.step(Event::JoinFailed { reason: FailReason::Other }, t);
    }
    p.step(Event::ApClientAssociated, 46_000);
    let a = p.step(Event::Joined { ip: LAN }, 50_000);
    assert_eq!(a.as_slice(), [Action::Announce]);
    assert!(p.ap_up());
    let a = p.step(Event::Tick, 50_000 + Timing::SPEC.ap_grace_ms);
    assert_eq!(a.as_slice(), [Action::DropAp]);
}

/// A late lease from a *failed portal trial* is not taken: the page already
/// said those credentials failed, and nothing may be committed behind its
/// back. The next retry puts the station back on the stored pair.
#[test]
fn a_late_lease_from_a_failed_trial_is_not_taken() {
    let mut p = Provisioner::new(&Config { has_stored: true, timing: Timing::SPEC, ..Config::default() });
    p.step(Event::Boot, 0);
    for t in [15_000, 30_000, 45_000] {
        p.step(Event::JoinFailed { reason: FailReason::Other }, t);
    }
    p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 50_000);
    p.step(Event::JoinFailed { reason: FailReason::AuthError }, 52_000);
    assert_eq!(p.state(), State::Portal);
    assert!(p.step(Event::Joined { ip: LAN }, 53_000).is_empty());
    assert_eq!(p.state(), State::Portal);
}

/// The portal's retry runs every 30 s, so it is most of the portal's life.
/// The QR and the `PROVISIONING` overlay stay up through it, and a post
/// that arrives during it is a portal trial (AP kept, failure back to the
/// page) rather than an online one.
#[test]
fn the_portal_survives_its_own_retry() {
    let mut p = Provisioner::new(&Config { has_stored: true, timing: Timing::SPEC, ..Config::default() });
    p.step(Event::Boot, 0);
    for t in [15_000, 30_000, 45_000] {
        p.step(Event::JoinFailed { reason: FailReason::NetworkNotFound }, t);
    }
    assert_eq!(p.state(), State::Portal);
    let a = p.step(Event::Tick, 45_000 + Timing::SPEC.portal_retry_ms);
    assert_eq!(a.as_slice(), [Action::StartJoin { which: JoinTarget::Stored, attempt: 1 }]);
    assert_eq!(p.state(), State::Joining);
    assert!(matches!(p.screen(80_000), Some(Screen::Portal { .. })), "the QR stays on the panel");
    assert_eq!(p.overlay_state(), Some(screeny_proto::control::state::PROVISIONING));

    let a = p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 81_000);
    assert_eq!(a.as_slice(), [Action::StopJoin, Action::StartJoin { which: JoinTarget::Trial, attempt: 1 }]);
    assert_eq!(p.trial_origin(), Some(screeny_provision::TrialOrigin::Portal));
    assert!(p.ap_up());
    p.step(Event::JoinFailed { reason: FailReason::AuthError }, 83_000);
    assert_eq!(p.state(), State::Portal, "a failed portal trial goes back to the portal page");
}

/// An online device's join (the `link_down_ms` rejoin) raises no AP and shows
/// no portal: `Joining` keeps its old meaning when the AP is down.
#[test]
fn an_online_rejoin_is_not_the_portal() {
    let mut w = World::online();
    w.outage = (w.now + 1_000, w.now + 200_000);
    while w.p.state() != State::Joining {
        w.second();
    }
    assert!(!w.p.ap_up());
    assert_eq!(w.p.screen(w.now), None);
    assert_eq!(w.p.overlay_state(), None);
}
