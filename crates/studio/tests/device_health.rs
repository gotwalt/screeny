//! Card 195: the rows nobody ever sees, seen.
//!
//! The four things card 180 made stand out, plus the two levels of free stack
//! and the reboot the studio did not ask for, driven against a **running**
//! simulator with `SimHandle::set_health` (card 192) rather than against a
//! hand-written server. The point of the simulator here is that these are the
//! numbers a device-shaped thing reports over the same HTTP the panel serves:
//! nothing in this file constructs a `DeviceFacts` by hand.
//!
//! The thresholds themselves are the firmware session's, measured on the real
//! device, and they live in `devices.rs` beside their reasoning. What is
//! pinned here is which side of them a given reading falls on.
//!
//! **No test in this file may touch the bench device.** Loopback only, ports
//! well above the spec's, mDNS off, and every wait has a deadline - and every
//! wait is for the read that carries the *new number*, so the verdicts are
//! asserted out of the same moment that produced them.

mod common;

use common::{post, until_json};
use std::net::SocketAddr;
use std::time::Duration;

use screeny_device_api::{FwState, ResetReason};
use screeny_sim::{Config as SimConfig, Health, SimDevice};
use screeny_studio::{Config, Running, Studio};

/// Long enough for a loaded bench, short enough that a wedged test is a test
/// failure rather than a hung suite.
const PATIENCE: Duration = Duration::from_secs(30);

/// The real panel's healthy readings, as the orchestrator measured them on the
/// live service (card 180's log, 2026-09-20): firmware 0.4.3, 45612 of 90112
/// bytes of heap (51%) and 20272 bytes of stack never touched.
const REAL_PANEL: Health = Health {
    reset_reason: ResetReason::PowerOn,
    fw_slot: screeny_device_api::FwSlot::Ota0,
    fw_state: FwState::Valid,
    store_errors: 0,
    heap_used: 45_612,
    heap_size: 90_112,
    stack_free: 20_272,
};

/// Where one simulator is: its frame port and its HTTP port.
#[derive(Clone, Copy, Debug)]
struct Ports {
    frame: u16,
    http: u16,
}

/// A simulator on loopback, healthy, with its own HTTP API on a known port.
/// Everything is well above 49374/49375, so a stray packet cannot reach the
/// bench device even in principle.
fn sim_at(ports: Ports) -> std::io::Result<SimDevice> {
    let cfg = SimConfig {
        frame_port: ports.frame,
        control_port: ports.frame + 1,
        http: true,
        http_port: ports.http,
        http_port_explicit: true,
        wifi_ssid: "simulated".to_string(),
        ..SimConfig::for_test()
    };
    SimDevice::start_with(cfg, None)
}

/// A studio, a simulator and the studio already reading its status.
struct Bench {
    dev: SimDevice,
    studio: Running,
    at: SocketAddr,
}

impl Bench {
    /// The first free set of ports, a simulator on it, a studio pointed at it,
    /// and one status read already done - so every later read has a previous
    /// one to compare a `boot_id` against.
    async fn start() -> Bench {
        let (dev, ports) = (0..40u16)
            .find_map(|i| {
                let ports = Ports { frame: 51_500 + i * 2, http: 51_600 + i };
                sim_at(ports).ok().map(|d| (d, ports))
            })
            .expect("no free simulator ports in 51500..51580 / 51600..51640");

        // Quickly is the one thing a test may do that the product may not: the
        // ten second floor is there for the *device's* one connection worker,
        // and this talks to a simulator on loopback with a thread per
        // connection. `main.rs` is what holds that line for the product.
        let cfg = Config {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            supervise_every: Duration::from_millis(200),
            telemetry_every: Duration::from_millis(200),
            device_http_every: Duration::from_millis(300),
            device_http_port: ports.http,
            ..Config::default()
        };
        let studio = Studio::bind(cfg).await.expect("bind an ephemeral loopback port").spawn();
        let at = studio.addr;

        let body = format!(r#"{{"to":"127.0.0.1:{}","name":"bench","play":false}}"#, ports.frame);
        let added = post(at, "/api/v1/devices/add", &body).await;
        assert_eq!(added.status, 200, "{}", String::from_utf8_lossy(&added.body));
        until_json(at, PATIENCE, "the first status read", "/api/v1/status", |v| {
            !v["devices"][0]["facts"].is_null()
        })
        .await;

        Bench { dev, studio, at }
    }

    /// The device's id, as the studio knows it.
    async fn device(&self) -> String {
        let v = until_json(self.at, PATIENCE, "the device's real id", "/api/v1/status", |v| {
            v["devices"][0]["resolved"] == true
        })
        .await;
        v["devices"][0]["id"].as_str().expect("an id").to_string()
    }

    /// Change what the simulator says about itself and wait for a status read
    /// that **began after the change** and carries all seven of its numbers.
    /// Returns that read's `facts`, so every verdict a test asserts comes out
    /// of the one moment that produced it.
    ///
    /// Both halves are needed. Waiting on the values alone would be satisfied
    /// by a stale read whenever the new health happens to share a field with
    /// the old (a brownout changes nothing but the reason); waiting on the
    /// read count alone would not know the simulator had caught up. The count
    /// is `+ 2` because the read in flight when `set_health` returns may have
    /// asked the simulator before it was told.
    async fn health(&self, what: &str, h: Health) -> serde_json::Value {
        let before = self.status().await["devices"][0]["http"]["reads"].as_u64().unwrap_or(0);
        self.dev.handle().set_health(h);
        // The API's own spelling of the three enums, not the Rust variant's.
        let want = serde_json::json!({
            "reset_reason": serde_json::to_value(h.reset_reason).expect("a reset reason"),
            "fw_slot": serde_json::to_value(h.fw_slot).expect("a slot"),
            "fw_state": serde_json::to_value(h.fw_state).expect("a slot state"),
            "store_errors": h.store_errors,
            "heap_used": h.heap_used,
            "heap_size": h.heap_size,
            "stack_free": h.stack_free,
        });
        let seen = until_json(self.at, PATIENCE, what, "/api/v1/status", |v| {
            let d = &v["devices"][0];
            d["http"]["reads"].as_u64().unwrap_or(0) >= before + 2
                && want.as_object().expect("an object").iter().all(|(k, value)| d["facts"][k] == *value)
        })
        .await;
        seen["devices"][0]["facts"].clone()
    }

    async fn status(&self) -> serde_json::Value {
        common::get(self.at, "/api/v1/status").await.json()
    }

    async fn finish(self) {
        drop(self.dev);
        self.studio.stop().await;
    }
}

/// **Free stack has two levels** (card 195). The firmware session's numbers:
/// warn below 8192, fault below 4096, because `stack_free` is a high-water
/// mark that only ever falls and interrupts eat it 256 bytes at a time.
///
/// 6000 is the margin going; 3000 is the margin gone; the healthy panel's
/// 20272 is neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stack_has_a_warning_level_and_a_fault_level() {
    let b = Bench::start().await;

    let f = b.health("a stack of 6000", Health { stack_free: 6000, ..Health::default() }).await;
    assert_eq!(f["stack_warn"], true, "6000 B is below the warning line: {f}");
    assert_eq!(f["stack_fault"], false, "...and above the fault line: {f}");
    assert_eq!(f["low_stack"], false, "the old name means the fault level now: {f}");

    let f = b.health("a stack of 3000", Health { stack_free: 3000, ..Health::default() }).await;
    assert_eq!(f["stack_fault"], true, "3000 B is below the fault line: {f}");
    assert_eq!(f["stack_warn"], true, "a fault is below the warning line too: {f}");
    assert_eq!(f["low_stack"], true, "the old name follows the fault level: {f}");

    // And back up: nothing sticks. (On the device it could not go back up -
    // it is a high-water mark - but the studio must not be the thing that
    // remembers a number the device has stopped reporting.)
    let f = b.health("the healthy reading again", REAL_PANEL).await;
    assert_eq!(f["stack_warn"], false, "20 KB is an ordinary row: {f}");
    assert_eq!(f["stack_fault"], false, "{f}");

    let seen = b.status().await;
    assert_eq!(seen["ok"], true, "a thin stack is the panel's problem, not the server's: {}", seen["problems"]);
    b.finish().await;
}

/// **A heap past 85% is a fault**, which is the firmware session's line:
/// steady state is 51% and the worst instant they measured, with the setup AP
/// up, is 60%. 90% is nowhere near either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_heap_past_the_line_is_a_fault() {
    let b = Bench::start().await;

    let f = b
        .health(
            "a heap at 90%",
            Health { heap_used: 88_474, heap_size: 98_304, ..Health::default() },
        )
        .await;
    assert_eq!(f["low_heap"], true, "90% of the heap is in use: {f}");

    // The worst instant the firmware session measured is not a fault: 60%.
    let f = b
        .health(
            "the worst measured instant, 60%",
            Health { heap_used: 54 * 1024, heap_size: 90_112, ..Health::default() },
        )
        .await;
    assert_eq!(f["low_heap"], false, "60% is what a panel with its setup AP up reads: {f}");

    let seen = b.status().await;
    assert_eq!(seen["ok"], true, "{}", seen["problems"]);
    b.finish().await;
}

/// A **brownout** is something that happened *to* the panel, and it keeps the
/// tone card 180 gave it. It is not a reboot the studio can count, either: the
/// simulator's `boot_id` has not changed, and `boot_id` is the only thing a
/// reboot is ever counted from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_brownout_stands_out_and_is_not_a_reboot_anybody_counted() {
    let b = Bench::start().await;
    let f = b
        .health(
            "a brownout reset",
            Health { reset_reason: ResetReason::Brownout, ..Health::default() },
        )
        .await;
    assert_eq!(f["reset_reason"], "brownout", "{f}");
    assert_eq!(f["odd_reset"], true, "a brownout is not a quiet reset: {f}");
    assert_eq!(f["reboots"], 0, "the boot id never changed: {f}");
    assert_eq!(f["unasked_reboots"], 0, "...so nothing rebooted, asked for or not: {f}");
    b.finish().await;
}

/// A settings-store error and a firmware slot that is not `valid`: the other
/// two of card 180's four, seen against a device-shaped thing for the first
/// time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_errors_and_a_slot_that_is_not_valid_stand_out() {
    let b = Bench::start().await;
    let f = b
        .health(
            "three store errors and a slot pending verification",
            Health { store_errors: 3, fw_state: FwState::PendingVerify, ..Health::default() },
        )
        .await;
    assert_eq!(f["store_errors"], 3, "{f}");
    assert_eq!(f["fw_state"], "pending_verify", "{f}");
    assert_eq!(f["bad_fw_state"], true, "a slot that is not valid stands out: {f}");

    let seen = b.status().await;
    assert_eq!(seen["ok"], true, "an unhappy settings store is not a *server* fault: {}", seen["problems"]);
    assert_eq!(common::get(b.at, "/healthz").await.status, 200);
    b.finish().await;
}

/// **A reboot the studio asked for is not one it wonders about.** The studio
/// asks through its own reboot control, writes the ask down, and consumes it
/// when the panel comes back with a new `boot_id`.
///
/// The simulator sets `reset_reason: software` on a `REBOOT`, exactly as the
/// device does - which is the whole difficulty: that is also what a crash
/// looks like, and the ask is the only thing that tells them apart.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_reboot_the_studio_asked_for_is_not_counted_against_the_panel() {
    let b = Bench::start().await;
    let id = b.device().await;

    let r = post(b.at, "/api/v1/device/reboot", &format!(r#"{{"device":"{id}","confirm":true}}"#)).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));

    let seen = until_json(b.at, PATIENCE, "the panel to come back", "/api/v1/status", |v| {
        v["devices"][0]["facts"]["reboots"].as_u64().unwrap_or(0) >= 1
    })
    .await;
    let f = &seen["devices"][0]["facts"];
    assert_eq!(f["reboots"], 1, "one reboot: {f}");
    assert_eq!(f["reset_reason"], "software", "which is what a device says after a REBOOT: {f}");
    assert_eq!(f["unasked_reboots"], 0, "and this studio asked for it: {f}");
    assert_eq!(f["odd_reset"], false, "a software reset is a quiet one: {f}");

    // The ask has been consumed: a second reboot, this time behind the
    // studio's back, is not excused by the first ask.
    behind_the_studios_back(&b).await;
    let seen = until_json(b.at, PATIENCE, "the second reboot", "/api/v1/status", |v| {
        v["devices"][0]["facts"]["reboots"].as_u64().unwrap_or(0) >= 2
    })
    .await;
    let f = &seen["devices"][0]["facts"];
    assert_eq!(f["reboots"], 2, "{f}");
    assert_eq!(f["unasked_reboots"], 1, "one ask excuses exactly one reboot: {f}");
    b.finish().await;
}

/// **A reboot nobody here asked for is named as such** - quietly, because a
/// reflash looks exactly the same from this side of the wire, and this
/// firmware cannot tell a panic from any other software reset.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_reboot_behind_the_studios_back_is_named_as_such() {
    let b = Bench::start().await;
    behind_the_studios_back(&b).await;

    let seen = until_json(b.at, PATIENCE, "the studio to notice the reboot", "/api/v1/status", |v| {
        v["devices"][0]["facts"]["reboots"].as_u64().unwrap_or(0) >= 1
    })
    .await;
    let f = &seen["devices"][0]["facts"];
    assert_eq!(f["reboots"], 1, "{f}");
    assert_eq!(f["unasked_reboots"], 1, "nobody here asked for this one: {f}");
    assert_eq!(f["reset_reason"], "software", "{f}");
    // Quiet: not a fault tone on the page, and never a reason for a 503.
    assert_eq!(f["odd_reset"], false, "it is still a quiet reset reason: {f}");
    assert_eq!(seen["ok"], true, "a panel that may have crashed is not a *server* fault: {}", seen["problems"]);
    assert_eq!(common::get(b.at, "/healthz").await.status, 200);
    b.finish().await;
}

/// Reboot the simulator over its control port directly, which is what "the
/// studio did not ask" means: the same `REBOOT` the studio would have sent,
/// sent by somebody else.
async fn behind_the_studios_back(b: &Bench) {
    let control = b.dev.control_addr();
    tokio::task::spawn_blocking(move || {
        let mut c = screeny::ControlClient::connect(control).expect("a control socket");
        c.set_timeout(Duration::from_millis(400));
        c.reboot().expect("the simulator reboots");
    })
    .await
    .expect("the blocking task finished");
}

/// **The acceptance's other half**: the healthy panel shows nothing but
/// ordinary rows. The simulator's own defaults, and then the real panel's
/// readings from the live service - ~20 KB of stack and 51% of the heap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_healthy_panel_has_no_unhappy_rows_at_all() {
    let b = Bench::start().await;

    for (what, h) in [("the simulator's defaults", Health::default()), ("the real panel's readings", REAL_PANEL)] {
        let f = b.health(what, h).await;
        for quiet in ["stack_warn", "stack_fault", "low_stack", "low_heap", "bad_fw_state", "odd_reset", "wifi_stale_failure"] {
            assert_eq!(f[quiet], false, "{what}: {quiet} should be false: {f}");
        }
        assert_eq!(f["store_errors"], 0, "{what}: {f}");
        assert_eq!(f["reboots"], 0, "{what}: {f}");
        assert_eq!(f["unasked_reboots"], 0, "{what}: {f}");
        assert_eq!(f["link_up"], true, "{what}: {f}");
    }

    let seen = b.status().await;
    assert_eq!(seen["ok"], true, "{}", seen["problems"]);
    assert_eq!(common::get(b.at, "/healthz").await.status, 200);
    b.finish().await;
}
