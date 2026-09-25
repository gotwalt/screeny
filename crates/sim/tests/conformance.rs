//! The wire-level conformance suite, run against an in-process simulator.
//!
//! This is card 080's other half. `screeny_probe::suite` is the catalogue of
//! rules pointed at the real panel; running it here, on
//! loopback, in `cargo test`, is what stops it rotting between bench sessions
//! and is why the wire-level assertions could come out of the other test
//! files in this directory.
//!
//! What it does **not** replace: the bit-exact pixel checks in `codecs.rs`,
//! the event-stream checks, the fault injection in `faults.rs` and
//! `telemetry.rs`, and the virtual-clock exactness of `core_rules.rs`. None
//! of those is visible from outside the device, which is exactly why they
//! stay in process.

mod common;

use screeny_probe::suite;
use screeny_sim::{Config, SimDevice};

#[test]
fn the_wire_level_suite_passes_against_the_simulator() {
    let dev = SimDevice::start(Config::for_test()).expect("bind loopback");

    let opts = suite::Opts {
        frame_addr: dev.frame_addr(),
        ctrl_addr: dev.control_addr(),
        only: None,
        // The two HOLD_MS rules are 13 s each of doing nothing; `core_rules.rs`
        // already pins that transition to the microsecond on a virtual clock,
        // so paying 26 s of wall time on every `cargo test` would buy nothing.
        // The bench run takes them with `--slow`.
        slow: false,
        // The simulator's cap is 255 by default, so the cap rule has nothing
        // to measure; `control.rs` checks clamping against a cap of 120.
        cap_probe: false,
        restore_idle: 0,
    };

    let summary = suite::run(&opts).expect("the suite ran");
    assert_eq!(
        summary.failed, 0,
        "{} of {} rules failed; the report above says which",
        summary.failed,
        summary.passed + summary.failed + summary.skipped
    );
    // A guard against the catalogue silently emptying itself: if a refactor
    // stopped registering rules, every assertion above would still hold.
    assert!(
        summary.passed >= 60,
        "only {} rules ran; the catalogue has shrunk",
        summary.passed
    );
    assert_eq!(
        summary.skipped, 3,
        "expected exactly the two --slow rules and the cap probe to be skipped"
    );
}
