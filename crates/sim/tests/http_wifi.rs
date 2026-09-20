//! Card 224 (delivering card 081): the WiFi join/portal life, end to end.
//!
//! A failed trial join over HTTP and over UDP `SET_WIFI`, a successful one,
//! the link going down, the portal screen on the panel, and the invariant
//! that holds through all of it: **no PSK anywhere**.
//!
//! Every wait is bounded. The provisioning timings are compressed by
//! `Config::wifi_timing`, so nothing here waits the device's real 15 seconds
//! for an attempt - that behaviour is `crates/provision`'s to test, and it
//! does, in microseconds.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::http;
use screeny_device_api::reply::{StatusReply, WifiReply};
use screeny_device_api::{route, FailReason, StreamState, WifiState};
use screeny_proto::control::{wifi_state, Reply, Request, SetWifi};
use screeny_sim::{Config, Event, SimDevice, SimHandle, WifiOutcome, WifiPhase, WifiTiming};

/// Long enough for a scripted join and a few 10 ms ticks, short enough that a
/// broken test fails rather than hangs.
const T: Duration = Duration::from_secs(5);

/// The `Example-Wifi1` / `password9` of `CLAUDE.md`. Never a real network.
const SSID: &str = "Example-Wifi1";
const PSK: &str = "password9";

/// Timings compressed so a whole three-attempt join fits in a test.
fn quick_timing() -> WifiTiming {
    WifiTiming {
        join_attempt_ms: 300,
        join_attempts: 3,
        trial_attempts: 3,
        ap_grace_ms: 200,
        portal_retry_ms: 1_000,
        link_down_ms: 200,
        connected_screen_ms: 1_000,
        screen_alternate_ms: 4_000,
    }
}

fn portal(outcome: WifiOutcome) -> SimDevice {
    SimDevice::start(Config {
        start_in_portal: true,
        wifi_outcome: outcome,
        wifi_join_ms: 40,
        wifi_timing: quick_timing(),
        ..Config::for_test()
    })
    .expect("bind loopback")
}

fn api(dev: &SimDevice) -> SocketAddr {
    dev.http_addr().expect("the HTTP API is on")
}

fn wait_phase(sim: &SimHandle, want: WifiPhase) -> bool {
    sim.wait_until(T, |s| s.wifi_phase == want).is_some()
}

// ---------------------------------------------------------------------------
// The default simulator has not changed
// ---------------------------------------------------------------------------

#[test]
fn the_default_simulator_is_online_from_its_first_instant() {
    // Card 224 may not change what the simulator does on UDP, and what it has
    // always done is answer `GET_WIFI` with an SSID and `CONNECTED`, from the
    // moment it starts. No 200 ms window of `CONNECTING` at boot.
    let dev = SimDevice::start(Config::for_test()).expect("bind loopback");
    let sim = dev.handle();
    let shot = sim.snapshot();
    assert_eq!(shot.wifi_phase, WifiPhase::Online);
    assert_eq!(shot.wifi_state, wifi_state::CONNECTED);
    assert!(!shot.ssid.is_empty());
    assert!(!shot.portal, "no soft-AP when the store has credentials");
    assert_eq!(shot.state_byte, screeny_proto::control::state::IDLE);

    let ctrl = common::Ctrl::new(sim.control_addr());
    let d = ctrl.call(&Request::GetWifi, 1).expect("GET_WIFI");
    let (_, body) = common::parse_reply(&d);
    let Ok(Reply::Wifi { ssid, state }) = body else {
        panic!("expected a Wifi reply")
    };
    assert!(!ssid.is_empty());
    assert_eq!(state, wifi_state::CONNECTED);
    dev.shutdown();
}

#[test]
fn a_factory_fresh_device_boots_into_the_portal() {
    let dev = portal(WifiOutcome::Ok);
    let sim = dev.handle();
    let shot = sim.snapshot();
    assert_eq!(shot.wifi_phase, WifiPhase::Portal);
    assert!(shot.portal, "the soft-AP is up");
    assert_eq!(shot.ssid, "", "an empty store has no SSID");
    // Spec section 6.7: `PROVISIONING` is an overlay on the state byte, and
    // it is up while the portal is.
    assert_eq!(
        shot.state_byte,
        screeny_proto::control::state::PROVISIONING,
        "the portal shows in telemetry"
    );
    let status: StatusReply = http::get(api(&dev), route::STATUS).parse();
    assert!(status.portal);
    assert_eq!(status.state, StreamState::Provisioning);
    dev.shutdown();
}

// ---------------------------------------------------------------------------
// The trial join, over HTTP
// ---------------------------------------------------------------------------

#[test]
fn a_failed_trial_join_over_http_leaves_the_store_untouched() {
    let dev = portal(WifiOutcome::Fail);
    let addr = api(&dev);
    let sim = dev.handle();
    let commits_before = sim.wifi_commits();
    assert_eq!(sim.stored_ssid(), None);

    // The reply goes out before the radio work (spec 8.2), so it says
    // "trying" and not what the join eventually did.
    let res = http::post_form(addr, route::WIFI, &format!("ssid={SSID}&psk={PSK}"));
    assert_eq!(res.status, 200);
    assert_eq!(res.json()["result"], "trying");

    // While it is being tried, the page's full-page reload sees `connecting`
    // with the SSID it typed.
    let trying: WifiReply = http::get(addr, route::WIFI).parse();
    assert!(
        matches!(trying.state, WifiState::Connecting | WifiState::Failed),
        "{trying:?}"
    );

    // ...and it lands on `failed` / `auth`, which is what the portal page
    // shows the person holding the phone.
    assert!(
        sim.wait_until(T, |s| s.wifi_phase == WifiPhase::Portal
            && s.wifi_state == wifi_state::FAILED)
            .is_some(),
        "the trial should have failed back to the portal"
    );
    let failed: WifiReply = http::get(addr, route::WIFI).parse();
    assert_eq!(failed.state, WifiState::Failed);
    assert_eq!(failed.reason, Some(FailReason::Auth));
    assert_eq!(failed.ssid.as_deref(), Some(SSID));
    assert_eq!(failed.ip, None);

    // **Nothing was written.** This is the invariant the whole trial design
    // exists for: a wrong password must not cost you the network you were on.
    assert_eq!(sim.stored_ssid(), None);
    assert_eq!(sim.wifi_commits(), commits_before);
    // The soft-AP never went down, so the phone on the portal saw the answer.
    assert!(sim.snapshot().portal);

    dev.shutdown();
}

#[test]
fn a_failed_join_over_udp_set_wifi_takes_the_same_path() {
    // Spec 8.2's `SET_WIFI` and `POST /api/v1/wifi` feed one machine, so the
    // same request over the other transport must end in the same state.
    let dev = portal(WifiOutcome::Fail);
    let sim = dev.handle();
    let mut cursor = sim.event_cursor();
    let ctrl = common::Ctrl::new(sim.control_addr());

    let d = ctrl
        .call(
            &Request::SetWifi(SetWifi {
                ssid: SSID,
                psk: PSK,
                persist: true,
            }),
            9,
        )
        .expect("SET_WIFI reply");
    // Spec 8.2: the reply comes back before the radio work, and it is the
    // ordinary `SET_WIFI` reply, not an error.
    assert!(matches!(common::parse_reply(&d).1, Ok(Reply::SetWifi)));

    // It is logged, with no field for the PSK. That event is unchanged from
    // before card 224.
    let ev = sim
        .wait_for(&mut cursor, T, |e| matches!(e, Event::SetWifi { .. }))
        .expect("SET_WIFI logged");
    match ev {
        Event::SetWifi { ssid, persist } => {
            assert_eq!(ssid, SSID);
            assert!(persist);
        }
        other => panic!("{other:?}"),
    }

    assert!(
        sim.wait_until(T, |s| s.wifi_state == wifi_state::FAILED)
            .is_some(),
        "SET_WIFI should have driven the same failed trial"
    );
    let reply = sim.wifi_reply();
    assert_eq!(reply.state, WifiState::Failed);
    assert_eq!(reply.reason, Some(FailReason::Auth));
    assert_eq!(sim.stored_ssid(), None, "nothing was written");

    dev.shutdown();
}

#[test]
fn a_successful_trial_join_commits_once_and_goes_online() {
    let dev = portal(WifiOutcome::Ok);
    let addr = api(&dev);
    let sim = dev.handle();
    let before = sim.wifi_commits();

    let res = http::post_form(addr, route::WIFI, &format!("ssid={SSID}&psk={PSK}"));
    assert_eq!(res.json()["result"], "trying");

    assert!(wait_phase(&sim, WifiPhase::Online), "the trial should join");
    let wifi: WifiReply = http::get(addr, route::WIFI).parse();
    assert_eq!(wifi.state, WifiState::Connected);
    assert_eq!(wifi.ssid.as_deref(), Some(SSID));
    assert!(wifi.ip.is_some(), "an address to find the device at");
    assert_eq!(wifi.reason, None);

    // The one commit in the whole machine, and it happened *after* the join.
    assert_eq!(sim.wifi_commits(), before + 1);
    assert_eq!(sim.stored_ssid().as_deref(), Some(SSID));

    let status: StatusReply = http::get(addr, route::STATUS).parse();
    assert_eq!(status.wifi_state, WifiState::Connected);
    assert_eq!(status.ssid.as_deref(), Some(SSID));
    assert!(status.ip.is_some());

    // The AP is held up for its grace window so the phone standing on the
    // portal can read the new address, then dropped.
    assert!(
        sim.wait_until(T, |s| !s.portal).is_some(),
        "the soft-AP should come down after the grace window"
    );
    dev.shutdown();
}

#[test]
fn a_radio_that_never_answers_times_the_attempt_out() {
    // `--wifi-result slow`: nothing comes back at all, so the machine's own
    // `join_attempt_ms` ends the attempt. `FailReason::Other`, not `Auth`.
    let dev = portal(WifiOutcome::Slow);
    let addr = api(&dev);
    let sim = dev.handle();
    let _ = http::post_form(addr, route::WIFI, &format!("ssid={SSID}&psk={PSK}"));
    assert!(
        sim.wait_until(T, |s| s.wifi_state == wifi_state::FAILED)
            .is_some(),
        "three attempts of 300 ms should have run out"
    );
    let wifi: WifiReply = http::get(addr, route::WIFI).parse();
    assert_eq!(wifi.state, WifiState::Failed);
    assert_eq!(wifi.reason, Some(FailReason::Other));
    dev.shutdown();
}

#[test]
fn the_outcome_can_be_changed_between_attempts() {
    // Card 081's "a join outcome the test or CLI chooses": a person who
    // mistyped their password, then got it right.
    let dev = portal(WifiOutcome::Fail);
    let addr = api(&dev);
    let sim = dev.handle();

    let _ = http::post_form(addr, route::WIFI, &format!("ssid={SSID}&psk=wrong"));
    assert!(sim
        .wait_until(T, |s| s.wifi_state == wifi_state::FAILED)
        .is_some());
    assert_eq!(sim.stored_ssid(), None);

    sim.set_wifi_outcome(WifiOutcome::Ok);
    let _ = http::post_form(addr, route::WIFI, &format!("ssid={SSID}&psk={PSK}"));
    assert!(wait_phase(&sim, WifiPhase::Online));
    assert_eq!(sim.stored_ssid().as_deref(), Some(SSID));
    dev.shutdown();
}

#[test]
fn credentials_posted_while_online_are_refused_rather_than_silently_dropped() {
    // `screeny_provision`'s machine honours `CredentialsPosted` only in
    // `Portal` and `Trial`. The simulator does not invent a transition it does
    // not have; it says so, with the state in the sentence. See card 224's log
    // - this is the one real gap the card found in `crates/provision`.
    let dev = SimDevice::start(Config::for_test()).expect("bind loopback");
    let addr = api(&dev);
    let res = http::post_form(addr, route::WIFI, &format!("ssid={SSID}&psk={PSK}"));
    assert_eq!(res.status, 503);
    assert_eq!(res.error_code(), "unavailable");
    assert!(
        res.json()["detail"].as_str().unwrap().contains("online"),
        "the detail should name the state: {}",
        res.text()
    );
    dev.shutdown();
}

#[test]
fn the_button_wipe_forgets_the_network_and_raises_the_portal() {
    let dev = SimDevice::start(Config {
        wifi_timing: quick_timing(),
        ..Config::for_test()
    })
    .expect("bind loopback");
    let sim = dev.handle();
    assert_eq!(sim.wifi_phase(), WifiPhase::Online);
    assert!(sim.stored_ssid().is_some());

    sim.wifi_wipe();
    assert_eq!(sim.wifi_phase(), WifiPhase::Portal);
    assert_eq!(sim.stored_ssid(), None);
    assert!(sim.snapshot().portal);
    // A wipe lands in `disconnected`, not `failed`: nothing failed.
    assert_eq!(sim.snapshot().wifi_state, wifi_state::DISCONNECTED);
    dev.shutdown();
}

// ---------------------------------------------------------------------------
// The link going down (spec section 7.3)
// ---------------------------------------------------------------------------

#[test]
fn the_link_going_down_releases_the_panel_and_the_idle_screen_says_so() {
    use screeny_proto::dec::codec;
    use screeny_proto::F_KEY;
    use screeny_sim::State;

    let dev = SimDevice::start(Config {
        wifi_timing: quick_timing(),
        // The stream timings compressed too: `HOLD_MS` is ten real seconds on
        // a device, and this test is about the transition, not the wait.
        timing: screeny_sim::Timing {
            hold_ms: 150,
            fade_ms: 0,
            ..screeny_sim::Timing::SPEC
        },
        ..Config::for_test()
    })
    .expect("bind loopback");
    let sim = dev.handle();
    let mut tx = common::Sender::new(dev.frame_addr());

    // Get the device streaming, so there is a lock to release.
    let (payload, _) = common::solid([200, 30, 60]);
    tx.send(codec::SOLID, F_KEY, &payload);
    assert!(
        sim.wait_for_frames(1, T).is_some(),
        "the simulator should be LIVE"
    );
    assert_eq!(sim.snapshot().state, State::Live);

    // Spec 7.3: "any | WiFi link down | HOLD".
    sim.set_link_down(true);
    assert_eq!(sim.snapshot().state, State::Hold, "LIVE -> HOLD");

    // ...and then the idle screen, which says the network is down rather than
    // printing an address the device can no longer be reached at.
    assert!(
        sim.wait_until(T, |s| s.state == State::Idle).is_some(),
        "HOLD -> IDLE after HOLD_MS"
    );
    let frame = sim.render_display();
    let lit = frame.chunks(3).filter(|px| px[0] > 180 && px[1] < 200).count();
    assert!(
        lit > 5,
        "expected the amber 'NO NETWORK' line on the idle screen, got {lit} px"
    );

    // Research 007 section 5.2: after `link_down_ms` the machine gives up and
    // starts joining again.
    assert!(
        sim.wait_until(T, |s| s.wifi_phase != WifiPhase::Online)
            .is_some(),
        "the machine should go back to joining"
    );

    // And the link coming back before that would have been forgiven: a fresh
    // device, because this one has already moved on.
    dev.shutdown();
}

#[test]
fn a_link_that_comes_straight_back_costs_nothing() {
    let dev = SimDevice::start(Config {
        wifi_timing: WifiTiming {
            link_down_ms: 60_000,
            ..quick_timing()
        },
        ..Config::for_test()
    })
    .expect("bind loopback");
    let sim = dev.handle();
    sim.set_link_down(true);
    sim.set_link_down(false);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        sim.wifi_phase(),
        WifiPhase::Online,
        "a flap under link_down_ms does not cost the join"
    );
    assert!(sim.snapshot().wifi_state == wifi_state::CONNECTED);
    dev.shutdown();
}

// ---------------------------------------------------------------------------
// The panel
// ---------------------------------------------------------------------------

#[test]
fn in_the_portal_the_panel_is_the_provision_crates_own_screen() {
    // Not "looks like": the same bytes. The window and the `--dump-dir` PNGs
    // therefore show exactly what the device will show, down to the QR's
    // polarity and its three-pixel lit quiet zone.
    let ap = "screeny-4a00a4";
    let dev = SimDevice::start(Config {
        start_in_portal: true,
        ap_ssid: ap.into(),
        wifi_outcome: WifiOutcome::Slow,
        ..Config::for_test()
    })
    .expect("bind loopback");
    let sim = dev.handle();
    assert_eq!(sim.wifi_phase(), WifiPhase::Portal);

    let shown = sim.render_display();

    // Rebuild the same screen from `screeny_provision` alone and compare. The
    // two layouts alternate on a timer, so whichever of them the device is
    // showing, one of these two is it.
    let mut a = [0u8; screeny_proto::NBYTES];
    let mut b = [0u8; screeny_proto::NBYTES];
    for (layout, buf) in [
        (screeny_provision::Layout::QrAndName, &mut a),
        (screeny_provision::Layout::Text, &mut b),
    ] {
        screeny_provision::render(
            &screeny_provision::Screen::Portal {
                ssid: ap,
                layout,
                form: screeny_provision::UriForm::NoPass,
            },
            buf,
        )
        .expect("the portal screen renders");
    }
    assert!(
        shown[..] == a[..] || shown[..] == b[..],
        "the panel is not one of screeny_provision's two portal layouts"
    );
    // ...and it is not the ordinary status screen.
    assert_ne!(shown[..], [0u8; screeny_proto::NBYTES][..]);
    dev.shutdown();
}

// ---------------------------------------------------------------------------
// Spec section 8.4: the PSK, nowhere
// ---------------------------------------------------------------------------

#[test]
fn the_psk_is_in_no_reply_no_event_and_no_log_line() {
    let dev = portal(WifiOutcome::Ok);
    let addr = api(&dev);
    let sim = dev.handle();
    let cursor = sim.event_cursor();
    let secret = "correct-horse-battery-staple";

    // Post it over HTTP...
    let posted = http::post_form(addr, route::WIFI, &format!("ssid={SSID}&psk={secret}"));
    // ...and over UDP, so both paths are covered by one grep.
    let ctrl = common::Ctrl::new(sim.control_addr());
    let set_wifi = ctrl
        .call(
            &Request::SetWifi(SetWifi {
                ssid: SSID,
                psk: secret,
                persist: true,
            }),
            5,
        )
        .expect("SET_WIFI reply");
    assert!(wait_phase(&sim, WifiPhase::Online));

    // Every byte the simulator produced about it.
    let get_wifi = ctrl.call(&Request::GetWifi, 6).expect("GET_WIFI");
    let mut haystack: Vec<Vec<u8>> = vec![
        posted.body.clone(),
        set_wifi.clone(),
        get_wifi.clone(),
        http::get(addr, route::WIFI).body,
        http::get(addr, route::STATUS).body,
        http::get(addr, route::TELEMETRY).body,
        http::get(addr, route::NETWORKS).body,
        sim.info_bytes(),
    ];
    // ...the whole event log, as the binary would print it...
    let (_, events) = sim.events(cursor);
    assert!(!events.is_empty(), "there should be events to check");
    for e in &events {
        haystack.push(format!("{e:?}").into_bytes());
    }
    // ...and the debug rendering of the snapshot, which is what a person
    // dumps when something is wrong.
    haystack.push(format!("{:?}", sim.snapshot()).into_bytes());
    haystack.push(format!("{:?}", sim.wifi_reply()).into_bytes());

    for (i, bytes) in haystack.iter().enumerate() {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|w| w == secret.as_bytes()),
            "haystack {i} contains the PSK: {:?}",
            String::from_utf8_lossy(bytes)
        );
        // ...and no field is named for one either, which is what stops the
        // next person adding it.
        let lower = String::from_utf8_lossy(bytes).to_ascii_lowercase();
        assert!(!lower.contains("\"psk\""), "haystack {i} has a psk field");
        assert!(
            !lower.contains("password9"),
            "haystack {i} has the fixture password"
        );
    }

    // The SSID *is* there, which is the point: the invariant is about the
    // secret, not about the network's name.
    assert!(sim.stored_ssid().as_deref() == Some(SSID));
    dev.shutdown();
}

#[test]
fn the_http_event_log_records_the_path_but_never_the_query_or_the_body() {
    let dev = portal(WifiOutcome::Slow);
    let addr = api(&dev);
    let sim = dev.handle();
    let mut cursor = sim.event_cursor();

    let _ = http::post_form(addr, route::WIFI, &format!("ssid={SSID}&psk={PSK}"));
    let ev = sim
        .wait_for(&mut cursor, T, |e| matches!(e, Event::Http { .. }))
        .expect("an HTTP event");
    match ev {
        Event::Http {
            method,
            path,
            status,
        } => {
            assert_eq!(method, "POST");
            assert_eq!(path, route::WIFI);
            assert_eq!(status, 200);
        }
        other => panic!("{other:?}"),
    }
    dev.shutdown();
}
