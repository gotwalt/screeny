//! Every row of research 007 section 5.2's transition table, plus the cases
//! card 221 added because they are the ones that bite a device left alone for
//! months.
//!
//! No radio, no clock, no sleeping: the whole table runs in microseconds.

use screeny_provision::{
    Action, Config, Event, FailReason, JoinTarget, Provisioner, State, Timing, TrialOutcome,
    UriForm,
};
use screeny_proto::control::{state as tstate, wifi_state};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Test timings, small enough to step through by hand and each a different
/// number so a test cannot pass by confusing two of them.
const T: Timing = Timing {
    join_attempt_ms: 100,
    join_attempts: 3,
    trial_attempts: 3,
    ap_grace_ms: 300,
    portal_retry_ms: 1_000,
    link_down_ms: 600,
    connected_screen_ms: 700,
    screen_alternate_ms: 40,
};

fn cfg(stored: bool, builtin: bool) -> Config<'static> {
    Config {
        ap_ssid: "screeny-4a00a4",
        has_stored: stored,
        has_builtin: builtin,
        form: UriForm::NoPass,
        timing: T,
    }
}

fn booted(stored: bool, builtin: bool) -> (Provisioner, Vec<Action>) {
    let mut p = Provisioner::new(&cfg(stored, builtin));
    let a = p.step(Event::Boot, 0).as_slice().to_vec();
    (p, a)
}

/// Fail the attempt in flight `n` times in a row, collecting every action.
fn fail_n(p: &mut Provisioner, n: usize, reason: FailReason, mut now: u32) -> Vec<Action> {
    let mut out = Vec::new();
    for _ in 0..n {
        now += 1;
        out.extend(p.step(Event::JoinFailed { reason }, now));
    }
    out
}

/// Drive a device all the way to `Online` through the portal and a trial, the
/// way a person with a phone does. Returns the machine at `now = 1000`.
fn provisioned_through_the_portal() -> Provisioner {
    let (mut p, _) = booted(false, false);
    assert_eq!(p.state(), State::Portal);
    p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 500);
    p.step(Event::Joined { ip: [192, 168, 7, 221] }, 1_000);
    assert_eq!(p.state(), State::Online);
    p
}

// ---------------------------------------------------------------------------
// 007 section 5.2, row by row
// ---------------------------------------------------------------------------

/// Row 1: `BOOT` + store has credentials -> `JOINING`.
#[test]
fn boot_with_stored_credentials_joins() {
    let (p, actions) = booted(true, false);
    assert_eq!(p.state(), State::Joining);
    assert_eq!(
        actions,
        [Action::StartJoin {
            which: JoinTarget::Stored,
            attempt: 1
        }]
    );
    assert_eq!(p.wifi_state(), wifi_state::CONNECTING);
    assert!(!p.ap_up(), "no soft-AP while the stored credentials are tried");
}

/// Row 2: `BOOT` + store empty -> `PORTAL`.
#[test]
fn boot_with_an_empty_store_raises_the_portal() {
    let (p, actions) = booted(false, false);
    assert_eq!(p.state(), State::Portal);
    assert_eq!(actions, [Action::RaiseAp]);
    assert!(p.ap_up());
    // Nothing has failed: an empty store reads DISCONNECTED, not FAILED.
    assert_eq!(p.wifi_state(), wifi_state::DISCONNECTED);
    assert_eq!(p.overlay_state(), Some(tstate::PROVISIONING));
}

/// Row 3: `JOINING` + joined -> `ONLINE`, mDNS and the LAN server up.
#[test]
fn joining_that_succeeds_goes_online_and_announces() {
    let (mut p, _) = booted(true, false);
    let a = p.step(Event::Joined { ip: [192, 168, 7, 221] }, 40);
    assert_eq!(p.state(), State::Online);
    assert_eq!(
        a.as_slice(),
        [Action::Announce],
        "stored credentials are already stored: nothing to commit"
    );
    assert_eq!(p.wifi_state(), wifi_state::CONNECTED);
    assert_eq!(p.overlay_state(), None, "ONLINE is not a provisioning state");
    assert_eq!(p.ip(), Some([192, 168, 7, 221]));
}

/// Row 4: `JOINING` + 3 attempts failed -> `PORTAL`, `WIFI_STATE = FAILED`.
#[test]
fn three_failed_attempts_raise_the_portal() {
    let (mut p, _) = booted(true, false);
    let a = fail_n(&mut p, 3, FailReason::NetworkNotFound, 0);
    assert_eq!(p.state(), State::Portal);
    assert_eq!(
        a,
        [
            Action::StartJoin {
                which: JoinTarget::Stored,
                attempt: 2
            },
            Action::StartJoin {
                which: JoinTarget::Stored,
                attempt: 3
            },
            Action::RaiseAp,
        ]
    );
    assert_eq!(p.wifi_state(), wifi_state::FAILED);
    assert!(p.has_stored(), "a failed join does not erase the store");
}

/// Row 4, the timing half: three attempts that never answer at all take
/// `3 x join_attempt_ms`, which at [`Timing::SPEC`] is 007's "~45 s".
#[test]
fn a_silent_radio_reaches_the_portal_after_three_attempt_windows() {
    let mut p = Provisioner::new(&Config {
        timing: Timing::SPEC,
        ..cfg(true, false)
    });
    p.step(Event::Boot, 0);
    // One tick a second, as the firmware will.
    let mut now = 0;
    while p.state() == State::Joining && now < 120_000 {
        now += 1_000;
        p.step(Event::Tick, now);
    }
    assert_eq!(p.state(), State::Portal);
    assert_eq!(now, 45_000, "007 section 5.2's ~45 s");
}

/// Row 5: `PORTAL` + `POST /api/v1/wifi` -> `TRIAL`. Nothing is committed and
/// the AP stays up - that is the whole point of APSTA here.
#[test]
fn a_posted_credential_starts_a_trial_and_commits_nothing() {
    let (mut p, _) = booted(false, false);
    let a = p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 500);
    assert_eq!(p.state(), State::Trial);
    assert_eq!(
        a.as_slice(),
        [Action::StartJoin {
            which: JoinTarget::Trial,
            attempt: 1
        }]
    );
    assert!(p.ap_up(), "the AP is never dropped for a trial");
    assert!(!p.has_stored(), "nothing is committed before it works");
    assert_eq!(p.trial().unwrap().outcome, TrialOutcome::Trying);
    assert_eq!(p.trial().unwrap().ssid.as_str(), "Example-Wifi1");
    assert_eq!(p.wifi_state(), wifi_state::CONNECTING);
    assert_eq!(p.overlay_state(), Some(tstate::PROVISIONING));
}

/// Row 6: `TRIAL` + joined -> `ONLINE`; commit, hold the AP 30 s, then drop.
#[test]
fn a_trial_that_works_commits_then_drops_the_ap_after_the_grace_window() {
    let (mut p, _) = booted(false, false);
    p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 500);
    let a = p.step(Event::Joined { ip: [192, 168, 7, 221] }, 600);
    assert_eq!(p.state(), State::Online);
    assert_eq!(
        a.as_slice(),
        [
            Action::CommitCredentials {
                which: JoinTarget::Trial
            },
            Action::Announce,
        ]
    );
    assert!(p.has_stored());
    assert_eq!(p.trial().unwrap().outcome, TrialOutcome::Connected);
    assert_eq!(p.trial().unwrap().ip, Some([192, 168, 7, 221]));

    // The AP is still up while the page reports the new address.
    assert!(p.ap_up());
    assert!(p.step(Event::Tick, 600 + T.ap_grace_ms - 1).is_empty());
    assert!(p.ap_up());

    let a = p.step(Event::Tick, 600 + T.ap_grace_ms, );
    assert_eq!(a.as_slice(), [Action::DropAp]);
    assert!(!p.ap_up());
    // And it is dropped exactly once.
    assert!(p.step(Event::Tick, 10_000).is_empty());
}

/// Row 7: `TRIAL` + failed -> `PORTAL`, with a reason the page can show and
/// **nothing written to the store**.
#[test]
fn a_trial_that_fails_reports_why_and_leaves_the_store_alone() {
    // Start from a device that already has credentials, so "untouched" is a
    // claim with something to be untouched.
    let (mut p, _) = booted(true, false);
    fail_n(&mut p, 3, FailReason::NetworkNotFound, 0);
    assert_eq!(p.state(), State::Portal);
    assert!(p.has_stored());

    p.step(Event::CredentialsPosted { ssid: "Typo-Network" }, 100);
    let a = p.step(
        Event::JoinFailed {
            reason: FailReason::AuthError,
        },
        200,
    );
    assert_eq!(p.state(), State::Portal);
    assert!(
        a.is_empty(),
        "the AP never went down, so nothing has to be raised again"
    );
    assert!(p.ap_up());
    assert!(p.has_stored(), "a failed trial must not touch the store");
    let t = p.trial().unwrap();
    assert_eq!(t.outcome, TrialOutcome::Failed(FailReason::AuthError));
    assert_eq!(t.ssid.as_str(), "Typo-Network");
    assert_eq!(t.ip, None);
    assert_eq!(p.wifi_state(), wifi_state::FAILED);
}

/// The three failure reasons reach the page unchanged, in the words research
/// 007 section 7 gives `GET /api/v1/wifi`.
#[test]
fn every_failure_reason_reaches_the_page() {
    for (reason, word) in [
        (FailReason::AuthError, "auth"),
        (FailReason::NetworkNotFound, "not_found"),
        (FailReason::Other, "other"),
    ] {
        let (mut p, _) = booted(false, false);
        p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 10);
        // A transient reason is retried; drive it to the end either way.
        for i in 0..T.trial_attempts as u32 {
            p.step(Event::JoinFailed { reason }, 20 + i);
        }
        assert_eq!(p.state(), State::Portal);
        assert_eq!(p.trial().unwrap().outcome, TrialOutcome::Failed(reason));
        assert_eq!(reason.as_str(), word);
    }
}

/// Card 221's reading of 007's "TRIAL / 3 tries": a wrong password is
/// deterministic, so it is reported at once rather than making the person
/// holding the phone wait three attempt windows for the same answer.
/// Anything else is retried.
#[test]
fn an_auth_error_is_not_retried_but_a_transient_failure_is() {
    let (mut p, _) = booted(false, false);
    p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 10);
    let a = p.step(
        Event::JoinFailed {
            reason: FailReason::AuthError,
        },
        20,
    );
    assert_eq!(p.state(), State::Portal);
    assert!(a.is_empty());

    let (mut p, _) = booted(false, false);
    p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 10);
    let a = p.step(
        Event::JoinFailed {
            reason: FailReason::NetworkNotFound,
        },
        20,
    );
    assert_eq!(p.state(), State::Trial);
    assert_eq!(
        a.as_slice(),
        [Action::StartJoin {
            which: JoinTarget::Trial,
            attempt: 2
        }]
    );
}

/// Row 8: `PORTAL` + a retry tick with no station associated -> `JOINING`.
/// The 3 a.m. router reboot heals itself.
#[test]
fn the_portal_retries_the_stored_credentials_when_nobody_is_on_it() {
    let (mut p, _) = booted(true, false);
    fail_n(&mut p, 3, FailReason::NetworkNotFound, 0);
    assert_eq!(p.state(), State::Portal);

    // Not yet.
    assert!(p.step(Event::Tick, T.portal_retry_ms - 1).is_empty());
    assert_eq!(p.state(), State::Portal);

    let a = p.step(Event::Tick, T.portal_retry_ms + 10);
    assert_eq!(p.state(), State::Joining);
    assert_eq!(
        a.as_slice(),
        [Action::StartJoin {
            which: JoinTarget::Stored,
            attempt: 1
        }]
    );
    assert!(
        p.ap_up(),
        "the retry keeps the AP up: APSTA, so the portal survives it"
    );
    // And if it works this time, the AP comes down after the grace window.
    let a = p.step(Event::Joined { ip: [192, 168, 7, 221] }, T.portal_retry_ms + 20);
    assert_eq!(a.as_slice(), [Action::Announce]);
    let a = p.step(Event::Tick, T.portal_retry_ms + 20 + T.ap_grace_ms);
    assert_eq!(a.as_slice(), [Action::DropAp]);
}

/// Card 221: the retry is suppressed while a client is associated, because it
/// would cost whoever is typing a password a ~45 s outage.
#[test]
fn the_retry_is_suppressed_while_a_phone_is_on_the_portal() {
    let (mut p, _) = booted(true, false);
    fail_n(&mut p, 3, FailReason::Other, 0);
    p.step(Event::ApClientAssociated, 10);
    assert_eq!(p.ap_clients(), 1);

    assert!(p.step(Event::Tick, T.portal_retry_ms * 5).is_empty());
    assert_eq!(p.state(), State::Portal, "not while somebody is here");

    // The moment they leave, the overdue retry fires.
    p.step(Event::ApClientLeft, T.portal_retry_ms * 5 + 1);
    assert_eq!(p.ap_clients(), 0);
    let a = p.step(Event::Tick, T.portal_retry_ms * 5 + 2);
    assert_eq!(p.state(), State::Joining);
    assert_eq!(
        a.as_slice(),
        [Action::StartJoin {
            which: JoinTarget::Stored,
            attempt: 1
        }]
    );
}

/// Card 221: with an empty store there is nothing to retry, so the portal
/// just sits there. It has no time limit (device-web, the orchestrator's
/// default for 007's open question 4).
#[test]
fn an_empty_store_never_retries_and_the_portal_never_expires() {
    let (mut p, _) = booted(false, false);
    for i in 1..=20u32 {
        assert!(p.step(Event::Tick, i * T.portal_retry_ms).is_empty());
        assert_eq!(p.state(), State::Portal);
        assert!(p.ap_up());
    }
}

/// Row 9: `ONLINE` + the link down for more than 60 s -> `JOINING`.
#[test]
fn a_link_that_stays_down_goes_back_to_joining() {
    let mut p = provisioned_through_the_portal();
    p.step(Event::Tick, 1_000 + T.ap_grace_ms); // drop the AP first
    assert!(!p.ap_up());

    p.step(Event::LinkDown, 2_000);
    assert_eq!(p.state(), State::Online, "a blip is not a disconnection");
    assert!(p.step(Event::Tick, 2_000 + T.link_down_ms - 1).is_empty());
    assert_eq!(p.state(), State::Online);

    let a = p.step(Event::Tick, 2_000 + T.link_down_ms);
    assert_eq!(p.state(), State::Joining);
    assert_eq!(
        a.as_slice(),
        [Action::StartJoin {
            which: JoinTarget::Stored,
            attempt: 1
        }]
    );
    assert_eq!(p.ip(), None);
}

/// Row 9, the other half: a link that comes back inside the window is not a
/// disconnection at all, and the timer resets.
#[test]
fn a_link_that_comes_back_in_time_is_not_a_disconnection() {
    let mut p = provisioned_through_the_portal();
    p.step(Event::Tick, 1_000 + T.ap_grace_ms); // drop the AP first
    p.step(Event::LinkDown, 2_000);
    p.step(Event::LinkUp, 2_000 + T.link_down_ms - 1);
    assert!(p.step(Event::Tick, 2_000 + T.link_down_ms * 3).is_empty());
    assert_eq!(p.state(), State::Online);
    // And a second dip starts its own window rather than inheriting the first.
    p.step(Event::LinkDown, 10_000);
    p.step(Event::Tick, 10_000 + T.link_down_ms - 1);
    assert_eq!(p.state(), State::Online);
    p.step(Event::Tick, 10_000 + T.link_down_ms);
    assert_eq!(p.state(), State::Joining);
}

/// Row 10: `ONLINE` -> `JOINING` -> three failures -> `PORTAL`. "The device
/// says why on the panel rather than sulking."
#[test]
fn losing_a_network_for_good_ends_at_the_portal() {
    let mut p = provisioned_through_the_portal();
    p.step(Event::Tick, 1_000 + T.ap_grace_ms);
    p.step(Event::LinkDown, 2_000);
    p.step(Event::Tick, 2_000 + T.link_down_ms);
    assert_eq!(p.state(), State::Joining);

    let a = fail_n(&mut p, 3, FailReason::NetworkNotFound, 3_000);
    assert_eq!(p.state(), State::Portal);
    assert_eq!(*a.last().unwrap(), Action::RaiseAp);
    assert!(p.ap_up());
    assert_eq!(p.wifi_state(), wifi_state::FAILED);
    assert!(p.has_stored(), "still stored; they just do not work today");
}

/// Row 11: the button wipes credentials and raises the portal **from every
/// state**, card 202/231's only requirement of this machine.
#[test]
fn button_wipe_from_every_state() {
    // Boot.
    let mut p = Provisioner::new(&cfg(true, true));
    let a = p.step(Event::ButtonWipe, 5);
    assert_eq!(p.state(), State::Portal);
    assert_eq!(a.as_slice(), [Action::ClearCredentials, Action::RaiseAp]);
    assert!(!p.has_stored());

    // Joining: the join in flight is abandoned first.
    let (mut p, _) = booted(true, true);
    assert_eq!(p.state(), State::Joining);
    let a = p.step(Event::ButtonWipe, 5);
    assert_eq!(p.state(), State::Portal);
    assert_eq!(
        a.as_slice(),
        [Action::StopJoin, Action::ClearCredentials, Action::RaiseAp]
    );

    // Online: no AP is up, so it has to be raised.
    let mut p = provisioned_through_the_portal();
    p.step(Event::Tick, 1_000 + T.ap_grace_ms);
    assert!(!p.ap_up());
    let a = p.step(Event::ButtonWipe, 5_000);
    assert_eq!(p.state(), State::Portal);
    assert_eq!(a.as_slice(), [Action::ClearCredentials, Action::RaiseAp]);
    assert_eq!(p.ip(), None);
    assert_eq!(p.trial(), None, "the old trial result is not the new truth");

    // Portal: already there, and the AP is already up, so it is not raised
    // twice - but the credentials still go.
    let (mut p, _) = booted(true, false);
    fail_n(&mut p, 3, FailReason::Other, 0);
    assert!(p.has_stored());
    let a = p.step(Event::ButtonWipe, 500);
    assert_eq!(p.state(), State::Portal);
    assert_eq!(a.as_slice(), [Action::ClearCredentials]);
    assert!(!p.has_stored());
    assert_eq!(
        p.wifi_state(),
        wifi_state::DISCONNECTED,
        "a wipe is not a failure"
    );

    // Trial.
    let (mut p, _) = booted(false, false);
    p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 10);
    assert_eq!(p.state(), State::Trial);
    let a = p.step(Event::ButtonWipe, 20);
    assert_eq!(p.state(), State::Portal);
    assert_eq!(a.as_slice(), [Action::StopJoin, Action::ClearCredentials]);
}

// ---------------------------------------------------------------------------
// The compile-time fallback (spec 8.3 step 2, device-web decision 6)
// ---------------------------------------------------------------------------

/// Stored credentials first, compile-time ones second, portal last.
#[test]
fn compile_time_credentials_are_tried_after_the_stored_ones() {
    let (mut p, a) = booted(true, true);
    assert_eq!(
        a,
        [Action::StartJoin {
            which: JoinTarget::Stored,
            attempt: 1
        }]
    );
    let a = fail_n(&mut p, 3, FailReason::NetworkNotFound, 0);
    assert_eq!(
        a,
        [
            Action::StartJoin {
                which: JoinTarget::Stored,
                attempt: 2
            },
            Action::StartJoin {
                which: JoinTarget::Stored,
                attempt: 3
            },
            // Not the portal yet: the compile-time credentials are step 2.
            Action::StartJoin {
                which: JoinTarget::Builtin,
                attempt: 1
            },
        ]
    );
    assert_eq!(p.state(), State::Joining);

    let a = fail_n(&mut p, 3, FailReason::NetworkNotFound, 100);
    assert_eq!(*a.last().unwrap(), Action::RaiseAp);
    assert_eq!(p.state(), State::Portal);
}

/// An empty store in a build that has compile-time credentials tries them
/// rather than raising the portal, and a success **seeds the store** with
/// them - device-web decision 6, which is what makes a bench flash come
/// straight up and be provisioned afterwards.
#[test]
fn compile_time_credentials_seed_an_empty_store() {
    let (mut p, a) = booted(false, true);
    assert_eq!(p.state(), State::Joining);
    assert_eq!(
        a,
        [Action::StartJoin {
            which: JoinTarget::Builtin,
            attempt: 1
        }]
    );
    let a = p.step(Event::Joined { ip: [192, 168, 7, 221] }, 50);
    assert_eq!(
        a.as_slice(),
        [
            Action::CommitCredentials {
                which: JoinTarget::Builtin
            },
            Action::Announce,
        ]
    );
    assert!(p.has_stored());
}

/// A build with neither goes straight to the portal, which is what a public
/// repo's default build does.
#[test]
fn a_build_with_no_credentials_at_all_boots_to_the_portal() {
    let (p, a) = booted(false, false);
    assert_eq!(p.state(), State::Portal);
    assert_eq!(a, [Action::RaiseAp]);
}

// ---------------------------------------------------------------------------
// The things a device left alone for months runs into
// ---------------------------------------------------------------------------

/// `now_ms` is a `u32` of milliseconds, which wraps every 49.7 days. Every
/// comparison in the machine is wrapping, so a wrap must cost nothing.
#[test]
fn every_timer_survives_the_49_day_wrap() {
    const NEAR: u32 = u32::MAX - 50;

    // The join attempt window, straddling the wrap.
    let mut p = Provisioner::new(&cfg(true, false));
    p.step(Event::Boot, NEAR);
    assert!(p.step(Event::Tick, NEAR.wrapping_add(T.join_attempt_ms - 1)).is_empty());
    assert_eq!(p.state(), State::Joining);
    let a = p.step(Event::Tick, NEAR.wrapping_add(T.join_attempt_ms));
    assert_eq!(
        a.as_slice(),
        [Action::StartJoin {
            which: JoinTarget::Stored,
            attempt: 2
        }],
        "the attempt window must not be either instant or infinite across a wrap"
    );

    // The portal retry, straddling the wrap.
    let mut p = Provisioner::new(&cfg(true, false));
    p.step(Event::Boot, NEAR);
    fail_n(&mut p, 3, FailReason::Other, NEAR);
    assert_eq!(p.state(), State::Portal);
    // `fail_n` spent three milliseconds getting here, so the retry is due
    // three milliseconds later too.
    assert!(p
        .step(Event::Tick, NEAR.wrapping_add(T.portal_retry_ms + 2))
        .is_empty());
    p.step(Event::Tick, NEAR.wrapping_add(T.portal_retry_ms + 3));
    assert_eq!(p.state(), State::Joining);

    // The AP grace window and the link-down window, straddling the wrap.
    let mut p = Provisioner::new(&cfg(false, false));
    p.step(Event::Boot, NEAR);
    p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, NEAR + 1);
    p.step(Event::Joined { ip: [10, 0, 0, 5] }, NEAR + 2);
    assert!(p.step(Event::Tick, NEAR.wrapping_add(T.ap_grace_ms)).is_empty());
    let a = p.step(Event::Tick, NEAR.wrapping_add(2 + T.ap_grace_ms));
    assert_eq!(a.as_slice(), [Action::DropAp]);

    p.step(Event::LinkDown, NEAR.wrapping_add(100));
    assert!(p
        .step(Event::Tick, NEAR.wrapping_add(100 + T.link_down_ms - 1))
        .is_empty());
    p.step(Event::Tick, NEAR.wrapping_add(100 + T.link_down_ms));
    assert_eq!(p.state(), State::Joining);

    // And the screen's alternation, which divides rather than subtracts.
    let (p, _) = booted(false, false);
    assert!(p.screen(u32::MAX).is_some());
    assert!(p.screen(0).is_some());
}

/// A late tick delays a transition; it never loses one. The firmware's tick
/// shares core 0 with the frame path, so it will sometimes be late.
#[test]
fn a_very_late_tick_still_fires_every_timer() {
    let mut p = provisioned_through_the_portal();
    // One tick, an hour later: the AP grace and the link-down window have
    // both long expired.
    p.step(Event::LinkDown, 1_100);
    let a = p.step(Event::Tick, 3_600_000);
    assert_eq!(
        a.as_slice(),
        [
            Action::DropAp,
            Action::StartJoin {
                which: JoinTarget::Stored,
                attempt: 1
            }
        ]
    );
    assert_eq!(p.state(), State::Joining);
}

/// A second POST while the first is still being tried - the user spotted a
/// typo. 007's table does not cover it; card 221 restarts the trial with the
/// new name rather than ignoring the correction.
#[test]
fn a_second_post_during_a_trial_restarts_it() {
    let (mut p, _) = booted(false, false);
    p.step(Event::CredentialsPosted { ssid: "Typo-Network" }, 10);
    let a = p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 20);
    assert_eq!(p.state(), State::Trial);
    assert_eq!(
        a.as_slice(),
        [
            Action::StopJoin,
            Action::StartJoin {
                which: JoinTarget::Trial,
                attempt: 1
            }
        ]
    );
    assert_eq!(p.trial().unwrap().ssid.as_str(), "Example-Wifi1");
}

/// Events that mean nothing in the state they arrive in are ignored, not
/// mishandled. This is the "no panic on any input" posture `crates/proto`
/// takes, applied to a state machine: the radio will deliver a stale
/// `JoinFailed` after a `StopJoin`.
#[test]
fn events_that_do_not_apply_are_ignored() {
    let (mut p, _) = booted(false, false);
    assert_eq!(p.state(), State::Portal);
    for ev in [
        Event::Boot,
        Event::Joined { ip: [1, 2, 3, 4] },
        Event::JoinFailed {
            reason: FailReason::Other,
        },
        Event::LinkDown,
        Event::LinkUp,
    ] {
        assert!(p.step(ev, 100).is_empty(), "{ev:?} should be ignored in Portal");
        assert_eq!(p.state(), State::Portal);
    }
    assert!(p.ap_up());
    assert!(!p.has_stored());
}

/// Client counting cannot go negative or overflow, whatever the radio says.
#[test]
fn ap_client_counting_saturates_both_ways() {
    let (mut p, _) = booted(false, false);
    p.step(Event::ApClientLeft, 1);
    assert_eq!(p.ap_clients(), 0);
    for i in 0..300u32 {
        p.step(Event::ApClientAssociated, 10 + i);
    }
    assert_eq!(p.ap_clients(), u8::MAX);
    p.step(Event::ApClientLeft, 1_000);
    assert_eq!(p.ap_clients(), u8::MAX - 1);
}

/// An SSID longer than the store's 32 bytes is cut, not refused and not
/// overflowed. Spec 6.3 caps `SET_WIFI` at 32; an HTTP form does not.
#[test]
fn an_over_long_posted_ssid_is_cut_to_the_store_limit() {
    let (mut p, _) = booted(false, false);
    let long = "x".repeat(200);
    p.step(Event::CredentialsPosted { ssid: &long }, 10);
    assert_eq!(p.trial().unwrap().ssid.len(), screeny_provision::SSID_MAX);
}

// ---------------------------------------------------------------------------
// What the rest of the system reads
// ---------------------------------------------------------------------------

/// The `GET_WIFI` state byte and the telemetry overlay, state by state,
/// against research 007 section 5.2's mapping paragraph.
#[test]
fn the_state_bytes_match_007_section_5_2() {
    let p = Provisioner::new(&cfg(true, false));
    assert_eq!(p.state(), State::Boot);
    assert_eq!(p.wifi_state(), wifi_state::DISCONNECTED);
    assert_eq!(p.overlay_state(), None);

    let (mut p, _) = booted(true, false);
    assert_eq!(p.wifi_state(), wifi_state::CONNECTING); // JOINING
    assert_eq!(p.overlay_state(), None);

    p.step(Event::Joined { ip: [1, 2, 3, 4] }, 10);
    assert_eq!(p.wifi_state(), wifi_state::CONNECTED); // ONLINE
    assert_eq!(p.overlay_state(), None);

    let (mut p, _) = booted(false, false);
    assert_eq!(p.wifi_state(), wifi_state::DISCONNECTED); // PORTAL, empty store
    assert_eq!(p.overlay_state(), Some(tstate::PROVISIONING));

    p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 10);
    assert_eq!(p.wifi_state(), wifi_state::CONNECTING); // TRIAL
    assert_eq!(p.overlay_state(), Some(tstate::PROVISIONING));

    p.step(
        Event::JoinFailed {
            reason: FailReason::AuthError,
        },
        20,
    );
    assert_eq!(p.wifi_state(), wifi_state::FAILED); // PORTAL after a failure
    assert_eq!(p.overlay_state(), Some(tstate::PROVISIONING));
}

/// The panel: the two portal layouts alternate, and the acquired address is
/// shown for `connected_screen_ms` after a successful trial and then handed
/// back to the normal idle path.
#[test]
fn the_panel_shows_the_portal_then_the_address_then_nothing() {
    use screeny_provision::{Layout, Screen};

    let (mut p, _) = booted(false, false);
    // Alternating on the caller's clock, `screen_alternate_ms` each.
    let layout_at = |p: &Provisioner, t: u32| match p.screen(t) {
        Some(Screen::Portal { layout, ssid, .. }) => {
            assert_eq!(ssid, "screeny-4a00a4");
            layout
        }
        other => panic!("expected the portal screen, got {other:?}"),
    };
    assert_eq!(layout_at(&p, 0), Layout::QrAndName);
    assert_eq!(layout_at(&p, T.screen_alternate_ms - 1), Layout::QrAndName);
    assert_eq!(layout_at(&p, T.screen_alternate_ms), Layout::Text);
    assert_eq!(layout_at(&p, T.screen_alternate_ms * 2), Layout::QrAndName);

    // The trial keeps the portal screen up: the phone is still on it.
    p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 100);
    assert!(matches!(p.screen(100), Some(Screen::Portal { .. })));

    // Then the address, for exactly `connected_screen_ms`.
    p.step(Event::Joined { ip: [192, 168, 7, 221] }, 200);
    assert_eq!(
        p.screen(200),
        Some(Screen::Connected {
            ip: [192, 168, 7, 221]
        })
    );
    assert!(p.screen(200 + T.connected_screen_ms - 1).is_some());
    assert_eq!(p.screen(200 + T.connected_screen_ms), None);
}

/// A join that was *not* a trial does not put an address on the panel: nobody
/// is standing there waiting for it, and the idle screen already shows the IP.
#[test]
fn a_boot_time_join_does_not_take_the_panel() {
    let (mut p, _) = booted(true, false);
    p.step(Event::Joined { ip: [192, 168, 7, 221] }, 10);
    assert_eq!(p.screen(10), None);
}

/// An AP name no version 2-L code can carry never gets the QR layout, so
/// `render` cannot fail on anything `screen()` hands it. (The AP name is
/// always `screeny-<id>`, so this is a guard, not a scenario.)
#[test]
fn a_name_that_cannot_be_a_qr_gets_the_text_layout_every_time() {
    use screeny_provision::{render, Layout, Screen};

    let mut p = Provisioner::new(&Config {
        ap_ssid: "screeny-a-very-long-name",
        ..cfg(false, false)
    });
    p.step(Event::Boot, 0);
    for t in 0..(T.screen_alternate_ms * 4) {
        match p.screen(t) {
            Some(Screen::Portal { layout, .. }) => assert_eq!(layout, Layout::Text),
            other => panic!("expected the portal screen, got {other:?}"),
        }
    }
    let mut frame = [0u8; screeny_proto::NBYTES];
    render(&p.screen(0).unwrap(), &mut frame).expect("screen() never yields an unrenderable screen");
}

/// `Debug` on the machine, on an action and on the trial result carries no
/// password - because no type in the crate has a field for one. The event
/// that brings credentials in carries the SSID alone.
#[test]
fn nothing_debug_printable_can_hold_a_psk() {
    let (mut p, _) = booted(false, false);
    let actions = p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 10);
    let dump = format!("{p:?} {actions:?} {:?}", p.trial());
    assert!(dump.contains("Example-Wifi1"), "the SSID is not a secret");
    assert!(!dump.contains("password9"), "and there is nowhere for a PSK to be");
}

// ---------------------------------------------------------------------------
// What this costs the firmware's RAM budget
// ---------------------------------------------------------------------------

/// Research 007 section 6 found core 0's stack down to 13.7 KB with
/// everything linked, and card 220 is spending its budget carefully. Nothing
/// in this crate is a `static`, so its whole cost is these two values plus
/// the QR encoder's two 80-byte scratch buffers inside `render`. If one of
/// these grows, the firmware wants to know before it flashes.
///
/// Measured today: `Provisioner` 168 B (two 32-byte `heapless::String`s - the
/// AP's name and the trial's - carry most of it), `Qr` 79 B, `Actions` 16 B.
#[test]
fn the_firmware_knows_exactly_what_this_crate_costs_it() {
    use core::mem::size_of;
    println!(
        "Provisioner {} B, Qr {} B, Actions {} B",
        size_of::<Provisioner>(),
        size_of::<screeny_provision::Qr>(),
        size_of::<screeny_provision::Actions>()
    );
    assert!(
        size_of::<Provisioner>() <= 192,
        "Provisioner is {} bytes",
        size_of::<Provisioner>()
    );
    assert!(
        size_of::<screeny_provision::Qr>() <= 80,
        "Qr is {} bytes",
        size_of::<screeny_provision::Qr>()
    );
    // An `Actions` batch is passed by value out of every `step`.
    assert!(
        size_of::<screeny_provision::Actions>() <= 32,
        "Actions is {} bytes",
        size_of::<screeny_provision::Actions>()
    );
}
