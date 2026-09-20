//! The HTTP conformance suite, run against an in-process simulator (card 228).
//!
//! This is the other half of `screeny_probe::http`, exactly as
//! `conformance.rs` is the other half of `screeny_probe::suite`: the
//! orchestrator points the same rules at the real device after a flash, and
//! running them here, on loopback, in `cargo test`, is what stops them rotting
//! between bench sessions.
//!
//! **Every rule runs here, including the ones the bench has to opt into.**
//! `--allow-reboot`, `--allow-wifi-trial` and `--cap-probe` are off by default
//! because of what they do to a real panel on a real network; a simulator has
//! neither, so the only honest thing to do is to run all of them. The device
//! this starts is shaped so that each of those rules has something to measure:
//!
//! * a brightness **cap of 100** under a starting brightness of 96, so the
//!   cap rule can ask for 255 and see 100 come back (the default simulator
//!   caps at 255, where that rule would prove nothing);
//! * the scripted radio set to [`WifiOutcome::Fail`] *after* the boot join, so
//!   the credentials the trial rules post are refused the way
//!   `Example-Wifi1` is refused on the bench - and the suite gets to assert
//!   the sticky `failed` and its `reason`;
//! * compressed provisioning timings, so the three attempts of a trial fit in
//!   a test rather than taking the device's real 45 seconds.
//!
//! Bounded: ephemeral ports, compressed timings, and every wait inside the
//! suite has its own deadline.

mod common;

use std::time::Duration;

use screeny_probe::http;
use screeny_sim::{Config, SimDevice, WifiOutcome, WifiPhase, WifiTiming};

/// Long enough for a scripted join and a few 10 ms ticks, short enough that a
/// broken test fails rather than hangs.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Timings compressed so a whole three-attempt trial fits in a test. The same
/// numbers `http_wifi.rs` uses.
fn quick_timing() -> WifiTiming {
    WifiTiming {
        join_attempt_ms: 300,
        join_attempts: 3,
        trial_attempts: 3,
        ap_grace_ms: 200,
        portal_retry_ms: 1_000,
        link_down_ms: 200,
        connected_screen_ms: 1_000,
    }
}

#[test]
fn the_http_suite_passes_against_the_simulator() {
    let dev = SimDevice::start(Config {
        // A cap the suite can actually see, and a starting brightness under
        // it: rule 18 steps down from 96, rule 19 asks for 255 and must get
        // 100 back.
        brightness: 96,
        brightness_cap: 100,
        wifi_join_ms: 40,
        wifi_timing: quick_timing(),
        ..Config::for_test()
    })
    .expect("bind loopback");
    assert_eq!(dev.handle().wifi_phase(), WifiPhase::Online, "booted joined");
    // ...and from here on every join attempt is refused, so the trial the
    // suite runs fails the way a made-up SSID fails on the bench.
    dev.handle().set_wifi_outcome(WifiOutcome::Fail);

    let http_addr = dev.http_addr().expect("the HTTP API is on by default");
    let opts = http::Opts {
        http_addr,
        // A bare IP literal is always "ours" to the captive-portal rule
        // (research 007 section 4.3), which is what a curl on the LAN sends.
        host: http_addr.to_string(),
        ctrl_addr: Some(dev.control_addr()),
        only: None,
        // The whole point: the bench has to opt into these, a test must not.
        allow_reboot: true,
        allow_wifi_trial: true,
        cap_probe: true,
        // The ctrl-c handler is process-wide and `cargo test` runs several
        // tests in one process. The `Drop` guard still restores.
        ctrlc: false,
    };

    let summary = http::run(&opts).expect("the suite ran");
    assert_eq!(
        summary.failed, 0,
        "{} of {} rules failed; the report above says which",
        summary.failed,
        summary.passed + summary.failed + summary.skipped
    );
    // A guard against the catalogue silently emptying itself: every assertion
    // above would still hold if no rule ran at all.
    assert!(
        summary.passed >= 30,
        "only {} rules ran; the catalogue has shrunk",
        summary.passed
    );

    dev.shutdown();
}

/// `--only` picks one rule out, by number and by section, and the suite still
/// restores what it touched.
#[test]
fn only_runs_what_it_was_asked_for() {
    let dev = SimDevice::start(Config::for_test()).expect("bind loopback");
    let http_addr = dev.http_addr().expect("the HTTP API is on");
    let base = http::Opts {
        ctrl_addr: Some(dev.control_addr()),
        ..http::Opts::new(http_addr, http_addr.to_string())
    };

    let one = http::Opts {
        only: Some("1".into()),
        ctrlc: false,
        ..base.clone()
    };
    let s = http::run(&one).expect("the suite ran");
    assert_eq!(s.passed + s.failed + s.skipped, 1);
    assert_eq!(s.failed, 0);

    let section = http::Opts {
        only: Some("status".into()),
        ctrlc: false,
        ..base.clone()
    };
    let s = http::run(&section).expect("the suite ran");
    assert!(s.passed >= 6, "the status section is eight rules");
    assert_eq!(s.failed, 0);

    let none = http::Opts {
        only: Some("nothing-is-called-this".into()),
        ctrlc: false,
        ..base
    };
    assert!(http::run(&none).is_err(), "an empty selection is an error");

    dev.shutdown();
}

/// Pointed at a port with nothing on it, the suite says so once instead of
/// failing forty rules.
#[test]
fn a_target_that_is_not_there_is_one_clear_error() {
    // A port that is bound and then dropped: nothing is listening, and the
    // connection is refused rather than filtered, so this cannot hang.
    let free = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = free.local_addr().expect("addr");
    drop(free);

    let opts = http::Opts {
        ctrlc: false,
        ..http::Opts::new(addr, addr.to_string())
    };
    let e = http::run(&opts).expect_err("nothing is there");
    assert!(e.contains("/api/v1/status"), "{e}");
}

/// Rule 8 against the one state that can break it: a device that fell back
/// onto its old network after a failed trial.
///
/// This is the state `docs/design/device-web.md`'s card 223 paragraph is
/// about. The sticky `failed` belongs to `GET /api/v1/wifi`; `status`'s
/// `wifi_state` is the **link**, which here is plainly up - the device is
/// online and holding an address. Firmware 0.4.0 reports the sticky value in
/// both places, and card 228 found that the simulator did too; the simulator
/// is fixed, so this rule is a `PASS` here while it is a known difference on
/// the device.
#[test]
fn the_sticky_failed_result_stays_off_the_status_route() {
    use screeny_proto::control::wifi_state;

    let dev = SimDevice::start(Config {
        wifi_join_ms: 40,
        wifi_timing: quick_timing(),
        ..Config::for_test()
    })
    .expect("bind loopback");
    let http_addr = dev.http_addr().expect("the HTTP API is on");
    let sim = dev.handle();

    // A trial that fails...
    sim.set_wifi_outcome(WifiOutcome::Fail);
    assert_eq!(sim.post_wifi("Typo-Network"), screeny_sim::Posted::Trial);
    assert!(
        sim.wait_until(TIMEOUT, |s| s.wifi_state == wifi_state::FAILED)
            .is_some(),
        "an auth error is not retried"
    );
    // ...and a fallback onto the stored network that works.
    sim.set_wifi_outcome(WifiOutcome::Ok);
    assert!(
        sim.wait_until(TIMEOUT, |s| s.wifi_phase == WifiPhase::Online)
            .is_some(),
        "back on the old network"
    );
    assert_eq!(
        sim.snapshot().wifi_state,
        wifi_state::FAILED,
        "the UDP GET_WIFI state is still the sticky one, and must stay that way"
    );

    let opts = http::Opts {
        ctrl_addr: Some(dev.control_addr()),
        only: Some("8".into()),
        ctrlc: false,
        ..http::Opts::new(http_addr, http_addr.to_string())
    };
    let s = http::run(&opts).expect("the suite ran");
    assert_eq!(
        s,
        http::Summary {
            passed: 1,
            failed: 0,
            skipped: 0,
            // Card 236: the simulator's server has a real listen backlog, so a
            // connect is never refused against it. The field is here to prove
            // that, not just to compile.
            refused: 0
        },
        "rule 8 should pass: the link is up, whatever the last trial did"
    );

    // ...and the sticky result really is still on the route it belongs to.
    let wifi: screeny_device_api::reply::WifiReply =
        common::http::get(http_addr, screeny_device_api::route::WIFI).parse();
    assert_eq!(wifi.state, screeny_device_api::WifiState::Failed);
    assert_eq!(wifi.ssid.as_deref(), Some("Typo-Network"));

    dev.shutdown();
}

/// The suite leaves the device as it found it, even when a rule changed
/// something on the way through.
#[test]
fn the_device_is_as_it_was_found_afterwards() {
    let dev = SimDevice::start(Config {
        name: "Bench panel".into(),
        brightness: 96,
        brightness_cap: 100,
        idle_mode: screeny_proto::control::IdleMode::Dim,
        ..Config::for_test()
    })
    .expect("bind loopback");
    let http_addr = dev.http_addr().expect("the HTTP API is on");
    let sim = dev.handle();
    let before = sim.snapshot();

    let opts = http::Opts {
        ctrl_addr: Some(dev.control_addr()),
        // The settings rules, and nothing that restarts or unjoins anything.
        only: Some("settings".into()),
        cap_probe: true,
        ctrlc: false,
        ..http::Opts::new(http_addr, http_addr.to_string())
    };
    let s = http::run(&opts).expect("the suite ran");
    assert_eq!(s.failed, 0);

    let after = sim.snapshot();
    assert_eq!(after.name, before.name, "the name came back");
    assert_eq!(
        after.brightness, before.brightness,
        "the brightness came back"
    );
    assert_eq!(after.idle_mode, before.idle_mode, "the idle mode came back");

    dev.shutdown();
}
