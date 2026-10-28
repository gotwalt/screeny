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

use screeny_probe::suite;
use screeny_sim::{Config, SimDevice};

/// The `--only` prefixes the catalogue is split into: the whole section where
/// a rule sits on the bare section number (a prefix of `"4"` takes `"4.1"` as
/// well, so the two cannot be told apart), and each subsection otherwise.
/// Every rule falls in exactly one; the test checks that rather than trusting
/// it.
fn groups(rules: &[suite::Rule]) -> Vec<String> {
    let bare: Vec<&str> = rules.iter().map(|r| r.section).filter(|s| !s.contains('.')).collect();
    let mut out: Vec<String> = rules
        .iter()
        .map(|r| {
            let major = r.section.split('.').next().unwrap_or(r.section);
            if bare.contains(&major) {
                major.to_string()
            } else {
                r.section.to_string()
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[test]
fn the_wire_level_suite_passes_against_the_simulator() {
    // One simulator per section, all at once. The rules are a second or so
    // each of real waiting - rate limits, LOCK_MS, the stream timeout - and
    // in a row they were 35 s of every `cargo test`. The rules already start
    // clean of each other (`Ctx::claim`); split by section, each gets a
    // device of its own as well.
    let rules = suite::all();
    let groups = groups(&rules);
    let summaries: Vec<(String, suite::Summary)> = std::thread::scope(|s| {
        let runs: Vec<_> = groups
            .iter()
            .map(|only| {
                s.spawn(move || {
                    let dev = SimDevice::start(Config::for_test()).expect("bind loopback");
                    let opts = suite::Opts {
                        frame_addr: dev.frame_addr(),
                        ctrl_addr: dev.control_addr(),
                        only: Some(only.clone()),
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
                    let summary =
                        suite::run(&opts).unwrap_or_else(|e| panic!("section {only}: the suite did not run: {e}"));
                    dev.shutdown();
                    (only.clone(), summary)
                })
            })
            .collect();
        runs.into_iter().map(|r| r.join().unwrap_or_else(|p| std::panic::resume_unwind(p))).collect()
    });
    let failed: Vec<&str> = summaries.iter().filter(|(_, s)| s.failed > 0).map(|(g, _)| g.as_str()).collect();
    let summary = suite::Summary {
        passed: summaries.iter().map(|(_, s)| s.passed).sum(),
        failed: summaries.iter().map(|(_, s)| s.failed).sum(),
        skipped: summaries.iter().map(|(_, s)| s.skipped).sum(),
    };
    assert_eq!(
        summary.passed + summary.failed + summary.skipped,
        rules.len(),
        "the sections {groups:?} do not cover each of the {} rules exactly once",
        rules.len()
    );
    assert_eq!(
        summary.failed,
        0,
        "{} of {} rules failed, in sections {failed:?}; the report above says which",
        summary.failed,
        summary.passed + summary.failed + summary.skipped
    );
    // A guard against the catalogue silently emptying itself: if a refactor
    // stopped registering rules, every assertion above would still hold.
    assert!(summary.passed >= 60, "only {} rules ran; the catalogue has shrunk", summary.passed);
    assert_eq!(summary.skipped, 3, "expected exactly the two --slow rules and the cap probe to be skipped");
}
