//! Card 192: the simulator can play a device that is **not** well.
//!
//! `GET /api/v1/status` carries four things nobody sees in the ordinary
//! course of things - a reset reason that is not a power-on, `store_errors`
//! above zero, an `fw_state` that is not `valid`, memory running out - and
//! before this card no simulator could produce any of them. These tests pin
//! the four, the defaults they replaced, and the one rule the simulator does
//! enforce about them (a `REBOOT` means `software` from then on).
//!
//! Everything here is loopback, ephemeral ports and in-process, except the
//! two tests that spawn the binary because the point *is* the flags. The one
//! that stays up carries `--exit-after` as a backstop and is killed when the
//! test is done; the refusals never get as far as binding a socket.

mod common;

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};

use common::http;
use screeny_device_api::reply::StatusReply;
use screeny_device_api::route;
use screeny_device_api::{FwSlot, FwState, ResetReason};
use screeny_sim::{Config, Health, SimDevice};

const BIN: &str = env!("CARGO_BIN_EXE_screeny-sim");

/// A device on loopback with ephemeral ports, and its HTTP address.
fn device(health: Health) -> (SimDevice, SocketAddr) {
    let dev = SimDevice::start(Config {
        health,
        ..Config::for_test()
    })
    .expect("bind loopback");
    let addr = dev.http_addr().expect("the HTTP API is on by default");
    (dev, addr)
}

fn status(addr: SocketAddr) -> StatusReply {
    http::get(addr, route::STATUS).parse()
}

/// Every one of the seven, set to something a healthy device would not say.
fn unwell() -> Health {
    Health {
        reset_reason: ResetReason::Brownout,
        fw_slot: FwSlot::Ota1,
        fw_state: FwState::PendingVerify,
        store_errors: 3,
        heap_used: 95_000,
        heap_size: 98_304,
        stack_free: 900,
    }
}

fn assert_status_is(s: &StatusReply, h: Health) {
    assert_eq!(s.reset_reason, h.reset_reason, "reset_reason");
    assert_eq!(s.fw_slot, h.fw_slot, "fw_slot");
    assert_eq!(s.fw_state, h.fw_state, "fw_state");
    assert_eq!(s.store_errors, h.store_errors, "store_errors");
    assert_eq!(s.heap_used, h.heap_used, "heap_used");
    assert_eq!(s.heap_size, h.heap_size, "heap_size");
    assert_eq!(s.stack_free, h.stack_free, "stack_free");
}

// ---------------------------------------------------------------------------
// The defaults, which are not allowed to move
// ---------------------------------------------------------------------------

/// The numbers are spelled out rather than compared against
/// `Health::default()`: this test exists to fail if somebody changes that
/// default, and a test written against it could not.
#[test]
fn the_defaults_are_exactly_what_status_reported_before_the_flags_existed() {
    let (dev, addr) = device(Health::default());
    let res = http::get(addr, route::STATUS);
    let s: StatusReply = res.parse();

    assert_eq!(s.reset_reason, ResetReason::PowerOn);
    assert_eq!(s.fw_slot, FwSlot::Ota0);
    assert_eq!(s.fw_state, FwState::Valid);
    assert_eq!(s.store_errors, 0);
    assert_eq!(s.heap_used, 64 * 1024, "64 KB of heap in use");
    assert_eq!(s.heap_size, 96 * 1024, "the firmware's 64 + 32 KB");
    assert_eq!(s.stack_free, 20 * 1024, "20 KB of stack never touched");

    // ...and on the wire, in the API's own names, because that is what a
    // browser and the Studio actually read.
    let body = res.text();
    for want in [
        r#""reset_reason":"power_on""#,
        r#""fw_slot":"ota_0""#,
        r#""fw_state":"valid""#,
        r#""store_errors":0"#,
        r#""heap_used":65536"#,
        r#""heap_size":98304"#,
        r#""stack_free":20480"#,
    ] {
        assert!(body.contains(want), "{want} is not in {body}");
    }

    // The library says the same thing the route does.
    assert_eq!(dev.handle().health(), Health::default());
    dev.shutdown();
}

// ---------------------------------------------------------------------------
// The flags
// ---------------------------------------------------------------------------

/// The binary, started once with all seven flags on ephemeral ports.
///
/// One spawn, because the point of this test is the wiring from the command
/// line to the reply; the variants are covered in-process below.
#[test]
fn every_flag_reaches_the_status_reply() {
    let run = Running::start(&[
        "--exit-after",
        "6",
        "--reset-reason",
        "brownout",
        "--fw-slot",
        "ota_1",
        "--fw-state",
        "pending_verify",
        "--store-errors",
        "3",
        "--heap-used",
        "95000",
        "--heap-size",
        "98304",
        "--stack-free",
        "900",
    ]);
    let s = status(run.http);
    assert_status_is(&s, unwell());
    // The banner said so too, and said it was only claiming.
    assert!(
        run.banner.contains("health:") && run.banner.contains("reported, not simulated"),
        "the banner did not mention the health: {:?}",
        run.banner
    );
    run.stop();
}

/// Every variant of all three enums, through the flag's own parser and out
/// again as JSON under the name it went in as.
///
/// One running device rather than twenty: the flag parser is a function, and
/// spawning a process per variant would buy nothing but twenty seconds.
#[test]
fn every_enum_variant_round_trips_through_its_flag() {
    let (dev, addr) = device(Health::default());
    let sim = dev.handle();

    for name in Health::reset_reason_names() {
        let parsed = Health::parse_reset_reason(&name).expect(&name);
        sim.set_health(Health {
            reset_reason: parsed,
            ..Health::default()
        });
        let res = http::get(addr, route::STATUS);
        assert_eq!(res.parse::<StatusReply>().reset_reason, parsed, "{name}");
        assert!(
            res.text().contains(&format!(r#""reset_reason":"{name}""#)),
            "{name} came back as something else: {}",
            res.text()
        );
    }

    for name in Health::fw_slot_names() {
        let parsed = Health::parse_fw_slot(&name).expect(&name);
        sim.set_health(Health {
            fw_slot: parsed,
            ..Health::default()
        });
        let res = http::get(addr, route::STATUS);
        assert_eq!(res.parse::<StatusReply>().fw_slot, parsed, "{name}");
        assert!(
            res.text().contains(&format!(r#""fw_slot":"{name}""#)),
            "{name} came back as something else: {}",
            res.text()
        );
    }

    for name in Health::fw_state_names() {
        let parsed = Health::parse_fw_state(&name).expect(&name);
        sim.set_health(Health {
            fw_state: parsed,
            ..Health::default()
        });
        let res = http::get(addr, route::STATUS);
        assert_eq!(res.parse::<StatusReply>().fw_state, parsed, "{name}");
        assert!(
            res.text().contains(&format!(r#""fw_state":"{name}""#)),
            "{name} came back as something else: {}",
            res.text()
        );
    }

    dev.shutdown();
}

/// A name the API does not have, and more heap in use than there is heap.
/// Both are refused before a socket is bound, so these spawns never listen.
#[test]
fn the_two_bad_command_lines_are_refused_with_a_message() {
    let cases: &[(&[&str], &[&str])] = &[
        // A misspelling gets the whole list back.
        (
            &["--reset-reason", "brownedout"],
            &["brownedout", "brownout", "power_on", "deep_sleep"],
        ),
        (&["--fw-slot", "ota_2"], &["ota_0", "ota_1", "unknown"]),
        (
            &["--fw-state", "fine"],
            &["pending_verify", "invalid", "undefined"],
        ),
        // A device cannot use more heap than it has.
        (
            &["--heap-used", "200000", "--heap-size", "98304"],
            &["--heap-used", "--heap-size"],
        ),
    ];
    for (args, wants) in cases {
        let out = Command::new(BIN).args(*args).output().expect("run");
        assert!(!out.status.success(), "{args:?} should have failed");
        let err = String::from_utf8_lossy(&out.stderr);
        for want in *wants {
            assert!(err.contains(want), "{args:?}: {want:?} missing from {err:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// On a running device
// ---------------------------------------------------------------------------

#[test]
fn set_health_changes_a_running_simulator() {
    let (dev, addr) = device(Health::default());
    let sim = dev.handle();
    assert_status_is(&status(addr), Health::default());

    sim.set_health(unwell());
    assert_status_is(&status(addr), unwell());
    assert_eq!(sim.health(), unwell(), "the handle reads back what it set");

    // ...and back again, because a test that turns a fault on has to be able
    // to turn it off.
    sim.set_health(Health::default());
    assert_status_is(&status(addr), Health::default());
    dev.shutdown();
}

/// Nothing else moved: this is a *report*, not a simulation. A brownout
/// reboots nothing, a `pending_verify` slot changes no behaviour, and the
/// device keeps streaming.
#[test]
fn an_unhealthy_status_changes_nothing_else_about_the_device() {
    let (dev, addr) = device(unwell());
    let sim = dev.handle();
    let before = status(addr);

    assert_eq!(before.boot_id, sim.boot_id(), "it did not restart");
    assert!(before.uptime_ms < 60_000, "the clock still starts at boot");
    let t = sim.telemetry();
    assert_eq!(t.frames_rejected, 0);

    // The settings path still works, which is the one that would notice a
    // store somebody had taught the simulator to believe was broken.
    let res = http::post_json(addr, route::SETTINGS, r#"{"brightness":96}"#);
    assert_eq!(res.status, 200, "{}", res.text());
    let after = status(addr);
    assert_eq!(after.brightness, 96);
    assert_status_is(&after, unwell());
    dev.shutdown();
}

// ---------------------------------------------------------------------------
// The one rule: a reboot means `software`
// ---------------------------------------------------------------------------

#[test]
fn a_reboot_over_http_makes_the_reset_reason_software() {
    let (dev, addr) = device(unwell());
    assert_eq!(status(addr).reset_reason, ResetReason::Brownout);

    let res = http::post_json(addr, route::REBOOT, r#"{"confirm":"RBOO"}"#);
    assert_eq!(res.status, 200, "{}", res.text());

    let after = status(addr);
    assert_eq!(
        after.reset_reason,
        ResetReason::Software,
        "a device that restarted because it was asked to says software"
    );
    // The other six are untouched: a reboot is not a memory event.
    assert_eq!(after.store_errors, 3);
    assert_eq!(after.stack_free, 900);
    assert_eq!(after.fw_slot, FwSlot::Ota1);

    // Pinning it again after the reboot wins, which is what lets a test show
    // the brownout row at all.
    dev.handle().set_health(unwell());
    assert_eq!(status(addr).reset_reason, ResetReason::Brownout);
    dev.shutdown();
}

#[test]
fn a_reboot_over_udp_makes_the_reset_reason_software_too() {
    let (dev, addr) = device(unwell());
    let ctrl = common::Ctrl::new(dev.control_addr());
    assert_eq!(status(addr).reset_reason, ResetReason::Brownout);

    ctrl.call(&screeny_proto::control::Request::Reboot, 7)
        .expect("REBOOT is answered");

    assert_eq!(
        status(addr).reset_reason,
        ResetReason::Software,
        "UDP REBOOT and POST /api/v1/reboot are one path, so they agree"
    );
    dev.shutdown();
}

/// An unconfirmed reboot is not a reboot, so it must not move the reason
/// either - the same shape as `http_routes.rs`'s `boot_id` rule.
#[test]
fn a_refused_reboot_leaves_the_reset_reason_alone() {
    let (dev, addr) = device(unwell());
    let res = http::post_json(addr, route::REBOOT, r#"{"confirm":"nope"}"#);
    assert_eq!(res.status, 400, "{}", res.text());
    assert_eq!(status(addr).reset_reason, ResetReason::Brownout);
    dev.shutdown();
}

// ---------------------------------------------------------------------------
// Running the binary
// ---------------------------------------------------------------------------

/// The binary on loopback with ephemeral ports, its HTTP address read off the
/// banner. `tests/cli.rs` does the same for the two UDP ports; this one wants
/// the second line.
struct Running {
    child: Child,
    http: SocketAddr,
    banner: String,
}

impl Running {
    fn start(extra: &[&str]) -> Running {
        let mut child = Command::new(BIN)
            .args([
                "--headless",
                "--no-mdns",
                "--bind",
                "127.0.0.1",
                "--frame-port",
                "0",
                "--control-port",
                "0",
                "--http-port",
                "0",
            ])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn screeny-sim");

        // `--quiet` is deliberately not passed: the banner is the evidence.
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut banner = String::new();
        let mut http = None;
        for _ in 0..8 {
            let mut line = String::new();
            if out.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            banner.push_str(&line);
            if let Some(rest) = line.split("http://").nth(1) {
                http = rest
                    .trim()
                    .trim_end_matches('/')
                    .parse::<SocketAddr>()
                    .ok();
            }
            if http.is_some() && banner.contains("health:") {
                break;
            }
        }
        Running {
            child,
            http: http.unwrap_or_else(|| panic!("no HTTP address in the banner: {banner:?}")),
            banner,
        }
    }

    /// Stop it now rather than waiting out `--exit-after`.
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
