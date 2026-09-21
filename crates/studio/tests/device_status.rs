//! Card 180: the studio reads the device's own `GET /api/v1/status`, and the
//! rules it has to keep while doing it.
//!
//! The device has **one connection worker and no listen backlog** (firmware
//! card 222): a second simultaneous connection is dropped at SYN. So the
//! interesting assertions here are not "the numbers arrive" but "the studio
//! never opens two connections at once", "a device with no HTTP server is
//! normal and silent", and "the SSID in the payload never leaves the page".
//!
//! **No test in this file may touch the bench device.** Loopback only, ports
//! well above the spec's, mDNS off, and every wait has a deadline.

mod common;

use common::{get, post, until, until_json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use screeny_sim::{Config as SimConfig, SimDevice};
use screeny_studio::{Config, Studio};

/// Long enough for a loaded bench, short enough that a wedged test is a test
/// failure rather than a hung suite.
const PATIENCE: Duration = Duration::from_secs(30);

/// Where one simulator is: its frame port and its HTTP port.
#[derive(Clone, Copy, Debug)]
struct Ports {
    frame: u16,
    http: u16,
}

/// A simulator on loopback with a **known** frame, control and HTTP port, so a
/// test can stop it and put another in its place - which is what "the panel
/// rebooted" looks like from the studio's side of the wire.
///
/// Consecutive frame/control ports on purpose: an `IP:PORT` target resolves
/// with the control port taken to be frame + 1. Everything is well above
/// 49374/49375, so a stray packet cannot reach the bench device even in
/// principle, and `http_port_explicit` means a busy port is an error here
/// rather than a silent ephemeral one somewhere else.
fn sim_at(ports: Ports, ssid: &str, http: bool) -> std::io::Result<SimDevice> {
    let cfg = SimConfig {
        frame_port: ports.frame,
        control_port: ports.frame + 1,
        http,
        http_port: ports.http,
        http_port_explicit: true,
        wifi_ssid: ssid.to_string(),
        ..SimConfig::for_test()
    };
    SimDevice::start_with(cfg, None)
}

/// The first free set of ports, and a simulator on it.
fn start_sim(ssid: &str, http: bool) -> (SimDevice, Ports) {
    for i in 0..60u16 {
        let ports = Ports { frame: 50_800 + i * 2, http: 51_000 + i };
        if let Ok(dev) = sim_at(ports, ssid, http) {
            return (dev, ports);
        }
    }
    panic!("no free simulator ports in 50800..50920 / 51000..51060");
}

/// A studio that reads its devices' status API on `http_port`, quickly.
///
/// Quickly is the one thing a test may do that the product may not: the ten
/// second floor is there for the *device's* one connection worker, and this
/// talks to a simulator on loopback with a thread per connection.
/// `main.rs::the_product_never_reads_a_panels_status_faster_than_the_floor`
/// is what holds that line.
async fn studio_reading(http_port: u16, every: Duration) -> screeny_studio::Running {
    let cfg = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        supervise_every: Duration::from_millis(200),
        telemetry_every: Duration::from_millis(200),
        device_http_every: every,
        device_http_port: http_port,
        ..Config::default()
    };
    Studio::bind(cfg).await.expect("bind an ephemeral loopback port").spawn()
}

/// Tell the studio about a simulator and wait until it knows its real id.
async fn attach(at: SocketAddr, ports: Ports) {
    let body = format!(r#"{{"to":"127.0.0.1:{}","name":"bench","play":false}}"#, ports.frame);
    let added = post(at, "/api/v1/devices/add", &body).await;
    assert_eq!(added.status, 200, "{}", String::from_utf8_lossy(&added.body));
    until_json(at, PATIENCE, "the studio to resolve the simulator", "/api/v1/status", |v| {
        v["devices"][0]["resolved"] == true
    })
    .await;
}

/// **The acceptance**, against the simulator: with the HTTP API on, the page
/// has heap, WiFi, the firmware slot and the reboot count - and they are the
/// device's own numbers rather than anything the studio could have invented.
///
/// Everything asserted comes out of the *one* read that satisfied the wait;
/// a second fetch would be a different moment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_http_the_page_has_what_only_the_device_knows() {
    let (dev, ports) = start_sim("simulated", true);
    let studio = studio_reading(ports.http, Duration::from_millis(300)).await;
    let at = studio.addr;
    attach(at, ports).await;

    // Both halves, out of one read: a wait satisfied by the HTTP facts alone
    // would be asserting on a moment when the UDP poll had not yet run, which
    // is a different moment from the one the test is about.
    let seen = until_json(at, PATIENCE, "the device's own status", "/api/v1/status", |v| {
        !v["devices"][0]["facts"].is_null() && !v["devices"][0]["telemetry"].is_null()
    })
    .await;
    let d = &seen["devices"][0];
    let f = &d["facts"];

    // The shapes are `screeny-device-api`'s, flattened: a field the firmware
    // adds arrives here without this crate restating it.
    assert_eq!(f["api"], 1, "{f}");
    assert_eq!(f["id"], "515151", "the simulator's own id: {f}");
    assert_eq!(f["heap_size"], 96 * 1024, "{f}");
    assert!(f["heap_used"].as_u64().expect("heap_used") <= 96 * 1024, "{f}");
    assert!(f["stack_free"].as_u64().expect("stack_free") > 0, "{f}");
    assert_eq!(f["fw_slot"], "ota_0", "{f}");
    assert_eq!(f["fw_state"], "valid", "{f}");
    assert_eq!(f["reset_reason"], "power_on", "{f}");
    assert_eq!(f["store_errors"], 0, "{f}");
    assert_eq!(f["wifi_state"], "connected", "{f}");
    assert_eq!(f["boot_id"], dev.handle().boot_id(), "the boot id is the device's own: {f}");
    assert!(f["ssid"].is_string(), "the network name is on the owner's page: {f}");
    assert!(f["ip"].is_string(), "{f}");

    // What only a studio that has watched more than one read can say.
    assert_eq!(f["reboots"], 0, "nothing has rebooted yet: {f}");
    assert_eq!(f["link_up"], true, "{f}");
    assert_eq!(f["wifi_stale_failure"], false, "{f}");
    assert_eq!(f["low_stack"], false, "the simulator is healthy: {f}");
    assert_eq!(f["low_heap"], false, "{f}");
    assert_eq!(f["bad_fw_state"], false, "{f}");
    assert_eq!(f["odd_reset"], false, "{f}");

    assert_eq!(d["http"]["absent"], false, "{}", d["http"]);
    assert_eq!(d["http"]["last_error"], serde_json::Value::Null, "{}", d["http"]);
    assert!(d["http"]["reads"].as_u64().expect("reads") >= 1, "{}", d["http"]);
    assert!(d["facts_ago"].as_f64().expect("an age") < 30.0, "{d}");

    // And the UDP half is untouched: merged, not replaced.
    assert!(!d["telemetry"].is_null(), "telemetry still comes from the control port: {d}");
    assert_eq!(seen["ok"], true, "{}", seen["problems"]);
    drop(dev);
    studio.stop().await;
}

/// A device with **no** HTTP server is normal: the page looks as it did
/// before, nothing complains, and `/healthz` never hears about it.
///
/// That is older firmware, and it is also the portable profile pointed at a
/// simulator started with `--no-http`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_http_nothing_complains() {
    let (dev, ports) = start_sim("simulated", false);
    assert!(dev.http_addr().is_none(), "this simulator serves no HTTP");
    let studio = studio_reading(ports.http, Duration::from_millis(300)).await;
    let at = studio.addr;
    attach(at, ports).await;

    let seen = until_json(at, PATIENCE, "the studio to notice there is no status API", "/api/v1/status", |v| {
        v["devices"][0]["http"]["absent"] == true && !v["devices"][0]["telemetry"].is_null()
    })
    .await;
    let d = &seen["devices"][0];
    assert!(d["facts"].is_null(), "there is nothing to show: {d}");
    assert!(d["facts_ago"].is_null(), "{d}");
    // The panel is still a panel: everything the page had before is here.
    assert!(!d["telemetry"].is_null(), "UDP telemetry carries on alone: {d}");
    assert_eq!(d["last_error"], serde_json::Value::Null, "a missing status API is not a control fault: {d}");

    assert_eq!(seen["ok"], true, "a panel with no HTTP server is not a problem: {}", seen["problems"]);
    assert!(
        seen["problems"].as_array().expect("problems").is_empty(),
        "{}",
        seen["problems"]
    );
    assert_eq!(get(at, "/healthz").await.status, 200);
    drop(dev);
    studio.stop().await;
}

/// **`boot_id` changing is the device rebooting**, and that is the only thing
/// the studio counts reboots from - never uptime, which is the agreement with
/// the firmware session.
///
/// The simulator draws a fresh `boot_id` at every start, so stopping one and
/// putting another on the same ports is a reboot as far as the wire can tell.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_that_reboots_is_counted_from_its_boot_id() {
    let (first, ports) = start_sim("simulated", true);
    let studio = studio_reading(ports.http, Duration::from_millis(300)).await;
    let at = studio.addr;
    attach(at, ports).await;

    let seen = until_json(at, PATIENCE, "the first status read", "/api/v1/status", |v| {
        !v["devices"][0]["facts"].is_null()
    })
    .await;
    let before = seen["devices"][0]["facts"]["boot_id"].as_u64().expect("a boot id");
    assert_eq!(seen["devices"][0]["facts"]["reboots"], 0);

    // Off and on again, at the same address.
    drop(first);
    let again = loop_until_bound(ports);

    let after = until_json(at, PATIENCE, "the studio to notice the reboot", "/api/v1/status", |v| {
        v["devices"][0]["facts"]["reboots"].as_u64().unwrap_or(0) >= 1
    })
    .await;
    let f = &after["devices"][0]["facts"];
    assert_eq!(f["reboots"], 1, "one reboot, seen once: {f}");
    assert_ne!(f["boot_id"].as_u64().expect("a boot id"), before, "a new boot draws a new id: {f}");
    assert_eq!(after["ok"], true, "a panel rebooting is not a server fault: {}", after["problems"]);

    // And a flap without a reboot does not count: several more reads of the
    // same device, and the number stays where it is.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let still = get(at, "/api/v1/status").await.json();
    assert_eq!(still["devices"][0]["facts"]["reboots"], 1, "reading it again is not a reboot");
    drop(again);
    studio.stop().await;
}

/// The port takes a moment to be free again after the first simulator goes.
fn loop_until_bound(ports: Ports) -> SimDevice {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        match sim_at(ports, "simulated", true) {
            Ok(dev) => return dev,
            Err(e) => {
                assert!(std::time::Instant::now() < deadline, "the ports never came free: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// ---------------------------------- card 118: a panel that is coming back ----

/// The poll period the two card 118 tests run at.
///
/// A whole second, which is slow for a test, because what they are about is a
/// *number of polls*: the cap is `MAX_BACKOFF` = 12 of them, so the two
/// behaviours - "back at once" and "back at the cap" - have to be a wall-clock
/// distance apart for a test to tell them apart at all.
const POLL: Duration = Duration::from_secs(1);

/// Generously more than the two or three polls the ladder costs, and less than
/// half the twelve the cap costs. Deliberately not tight: a loaded machine
/// slips ticks, and this must fail because the *rule* is wrong, not because the
/// scheduler was busy (card 093).
const BACK_WITHIN: Duration = Duration::from_secs(8);

/// **The card: a panel that has answered is not "a panel with no HTTP API".**
///
/// A panel reboots faster than it listens: the network stack is up, and frames
/// and UDP telemetry are flowing again, seconds before the HTTP workers accept
/// anything. Those seconds used to cost two minutes of a stale Device block,
/// because one refused connection said `absent` and `absent` jumped straight to
/// the cap.
///
/// Here the panel's UDP half never goes quiet - it is the simulator - and only
/// the status API goes away and comes back, on the same port. That isolates the
/// rule from everything else a reboot does.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_panel_that_has_answered_http_is_not_absent() {
    let server = CountingServer::start(Duration::ZERO);
    let port = server.addr.port();
    // The simulator is there to be *resolved* over UDP; the status reads are
    // pointed at this test's own server.
    let (dev, mut ports) = start_sim("simulated", false);
    ports.http = port;
    let studio = studio_reading(port, POLL).await;
    let at = studio.addr;
    attach(at, ports).await;

    let first = until_json(at, PATIENCE, "the first status read", "/api/v1/status", |v| {
        v["devices"][0]["http"]["reads"].as_u64().unwrap_or(0) >= 1
    })
    .await;
    let reads = first["devices"][0]["http"]["reads"].as_u64().expect("reads");

    // The panel starts rebooting: the port refuses.
    server.stop();
    let refused = until_json(at, PATIENCE, "the studio to see the connection refused", "/api/v1/status", |v| {
        !v["devices"][0]["http"]["last_error"].is_null()
    })
    .await;
    let h = &refused["devices"][0]["http"];
    assert_eq!(h["absent"], false, "a panel that has answered {reads} times is not a panel without the API: {h}");
    assert_eq!(refused["ok"], true, "a panel that is rebooting is not a server fault: {}", refused["problems"]);
    assert!(
        !refused["devices"][0]["facts"].is_null(),
        "the last thing it said about itself stays on the page: {}",
        refused["devices"][0]
    );

    // ...and finishes booting. The same port, because it is the same panel.
    let back = CountingServer::start_at(port, Duration::ZERO).expect("the status API comes back on its own port");
    let up = std::time::Instant::now();
    until_json(at, PATIENCE, "the studio to read the panel again", "/api/v1/status", |v| {
        v["devices"][0]["http"]["reads"].as_u64().unwrap_or(0) > reads
    })
    .await;
    let took = up.elapsed();
    println!(
        "card 118: the status API came back and was read again after {:.1} s ({:.1} polls of {:.1} s; the cap would be 12)",
        took.as_secs_f64(),
        took.as_secs_f64() / POLL.as_secs_f64(),
        POLL.as_secs_f64()
    );
    assert!(
        took < BACK_WITHIN,
        "a panel that had answered before waited {:.1} s ({:.1} polls) to be read again; the ladder from the bottom is two or three",
        took.as_secs_f64(),
        took.as_secs_f64() / POLL.as_secs_f64()
    );

    drop(dev);
    studio.stop().await;
    back.stop();
}

/// **The acceptance, in a simulator: reboot the panel and the Device block is
/// about the new boot within seconds**, not within two minutes.
///
/// Everything goes at once here, the way it does on the bench: UDP, HTTP, the
/// boot id and the uptime. The uptime coming back smaller is what the telemetry
/// poll notices, and it clears whatever the status poller had climbed to while
/// the panel was still booting.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_panel_that_rebooted_is_read_again_within_a_poll_or_two() {
    let (first, ports) = start_sim("simulated", true);
    let studio = studio_reading(ports.http, POLL).await;
    let at = studio.addr;
    attach(at, ports).await;

    // Enough uptime that the reboot is unambiguous in whole seconds - the
    // resolution telemetry reports it at.
    let seen = until_json(at, PATIENCE, "a panel that has been up for a few seconds", "/api/v1/status", |v| {
        !v["devices"][0]["facts"].is_null() && v["devices"][0]["telemetry"]["uptime_s"].as_u64().unwrap_or(0) >= 3
    })
    .await;
    let boot = seen["devices"][0]["facts"]["boot_id"].as_u64().expect("a boot id");

    // Off, which is a refused connection or two, and on again.
    drop(first);
    until_json(at, PATIENCE, "the studio to see the panel go away", "/api/v1/status", |v| {
        !v["devices"][0]["http"]["last_error"].is_null()
    })
    .await;
    let again = loop_until_bound(ports);
    let up = std::time::Instant::now();

    let after = until_json(at, PATIENCE, "the studio to read the new boot", "/api/v1/status", |v| {
        v["devices"][0]["facts"]["boot_id"].as_u64().unwrap_or(boot) != boot
    })
    .await;
    let took = up.elapsed();
    println!(
        "card 118: the panel rebooted and its facts were fresh again after {:.1} s ({:.1} polls of {:.1} s)",
        took.as_secs_f64(),
        took.as_secs_f64() / POLL.as_secs_f64(),
        POLL.as_secs_f64()
    );
    assert!(
        took < BACK_WITHIN,
        "the Device block was {:.1} s ({:.1} polls) behind the reboot: {}",
        took.as_secs_f64(),
        took.as_secs_f64() / POLL.as_secs_f64(),
        after["devices"][0]
    );
    assert_eq!(after["devices"][0]["http"]["absent"], false, "{}", after["devices"][0]["http"]);
    assert_eq!(after["ok"], true, "{}", after["problems"]);

    drop(again);
    studio.stop().await;
}

/// **One connection at a time.** The rule the device's one connection worker
/// and missing listen backlog make load-bearing: a second simultaneous
/// connection is dropped at SYN and costs a second of SYN retransmit.
///
/// A server that counts how many connections are open at once, and holds each
/// one open long enough that an overlap would be certain if the studio allowed
/// one. The poll period here is shorter than the hold time on purpose: if the
/// poller started a request per tick rather than waiting for the last, this
/// would see two immediately.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn the_studio_never_opens_a_second_connection_to_a_device() {
    // Hold each connection open for well over the poll period.
    let counter = CountingServer::start(Duration::from_millis(400));
    // The simulator is there to be *resolved* over UDP; its own HTTP server is
    // off, because the studio's status reads are pointed at the counter.
    let (dev, mut ports) = start_sim("simulated", false);
    ports.http = counter.addr.port();

    let studio = studio_reading(counter.addr.port(), Duration::from_millis(100)).await;
    let at = studio.addr;
    attach(at, ports).await;

    // Enough ticks that a poller which did not wait would have overlapped many
    // times over: 400 ms a request against a 100 ms period.
    until_json(at, PATIENCE, "several status reads", "/api/v1/status", |v| {
        v["devices"][0]["http"]["reads"].as_u64().unwrap_or(0) >= 5
    })
    .await;

    let (served, most, closed, hosts) = counter.report();
    println!("one-at-a-time: {served} requests served, at most {most} open at once, {closed} asked for Connection: close");
    assert!(served >= 5, "the studio only made {served} requests");
    assert_eq!(most, 1, "the studio had {most} connections open to one device at once");
    assert_eq!(closed, served, "every request must say Connection: close");
    assert!(hosts.iter().all(|h| h.contains(&counter.addr.port().to_string())), "the Host header names the device: {hosts:?}");
    drop(dev);
    studio.stop().await;
    counter.stop();
}

/// A reply longer than a status reply can be is refused rather than read into
/// this process: a wrong address that happens to be a web server must cost
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_endless_reply_is_refused_rather_than_read() {
    let flood = Flood::start();
    let addr = flood.addr;
    let read = tokio::task::spawn_blocking(move || {
        screeny_studio::devhttp::get_status(addr, screeny_studio::devhttp::TIMEOUT)
    })
    .await
    .expect("the blocking task finished");
    let fault = read.expect_err("an endless body is not a status");
    assert!(fault.absent, "something that floods is not this API: {fault}");
    println!("flood: refused with `{fault}` after {} bytes", flood.sent());
    flood.stop();
}

// -------------------------------------------- card 199: the panic route ----
//
// `screeny-sim`'s `GET /api/v1/panic` always answers `last_panic: null,
// last_reset: null` and never a `404` - the simulator has no crash path and
// card 243 landed the route in it - so these three behaviours need a server
// that can be told what to do on command, the same reason `CountingServer`
// and `Flood` above are hand-written rather than borrowed from `screeny-sim`.
// `PanicServer` below is that server: `GET /api/v1/status` with a movable
// `boot_id`, `GET /api/v1/panic` with a canned reply or a `404`, and a count
// of how many times each path was asked for.

/// **The heart of the card**: the panic route is asked exactly once while
/// `boot_id` stays put, however many times the status route is polled - and
/// what it said reaches `/api/v1/status` in the shape `crates/device-api`
/// defines, flattened, plus the one derived flag (`repeat`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_panic_route_is_read_once_per_boot_id_and_not_again() {
    let server = PanicServer::start(11);
    let port = server.addr.port();
    let (dev, mut ports) = start_sim("simulated", false);
    ports.http = port;
    let studio = studio_reading(port, Duration::from_millis(100)).await;
    let at = studio.addr;
    attach(at, ports).await;

    let seen = until_json(at, PATIENCE, "several status reads and one panic read", "/api/v1/status", |v| {
        v["devices"][0]["http"]["reads"].as_u64().unwrap_or(0) >= 5 && !v["devices"][0]["panic"].is_null()
    })
    .await;

    let p = &seen["devices"][0]["panic"];
    assert_eq!(p["boot_count"], 4, "{p}");
    assert_eq!(p["panic_count"], 2, "{p}");
    assert_eq!(p["last_panic"]["file"], "net.rs", "{p}");
    assert_eq!(p["last_panic"]["line"], 321, "{p}");
    assert_eq!(p["last_panic"]["consecutive"], 2, "{p}");
    assert_eq!(p["repeat"], true, "consecutive > 1 sets the flag the chip reads: {p}");
    assert!(seen["devices"][0]["panic_ago"].as_f64().expect("an age") < 10.0, "{}", seen["devices"][0]);

    assert!(server.status_hits() >= 5, "the studio only read status {} times", server.status_hits());
    assert_eq!(server.panic_hits(), 1, "the panic route must be asked exactly once while boot_id stays put");

    drop(dev);
    studio.stop().await;
    server.stop();
}

/// A `boot_id` change - the panel rebooted - is asked for its panic again;
/// a second one without a further reboot is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_boot_id_is_asked_for_its_panic_again() {
    let server = PanicServer::start(1);
    let port = server.addr.port();
    let (dev, mut ports) = start_sim("simulated", false);
    ports.http = port;
    let studio = studio_reading(port, Duration::from_millis(100)).await;
    let at = studio.addr;
    attach(at, ports).await;

    until_json(at, PATIENCE, "the first panic read", "/api/v1/status", |v| !v["devices"][0]["panic"].is_null()).await;
    assert_eq!(server.panic_hits(), 1, "the first boot_id is asked for once");

    server.set_boot_id(2);
    until_json(at, PATIENCE, "the studio to notice the new boot_id", "/api/v1/status", |v| {
        v["devices"][0]["facts"]["boot_id"] == 2
    })
    .await;
    until(PATIENCE, "a second panic read, for the new boot_id", || async { server.panic_hits() >= 2 }).await;
    assert_eq!(server.panic_hits(), 2, "a new boot_id is asked once, and only once, in its turn");

    drop(dev);
    studio.stop().await;
    server.stop();
}

/// **A 404 is silence, not a fault** (spec 8.6): firmware older than 0.5.2
/// serves no `PANIC` route, the page simply has nothing from it, and the
/// server is not asked again until the device reboots.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_404_from_the_panic_route_is_silence() {
    let server = PanicServer::start(1);
    server.set_panic_404(true);
    let port = server.addr.port();
    let (dev, mut ports) = start_sim("simulated", false);
    ports.http = port;
    let studio = studio_reading(port, Duration::from_millis(100)).await;
    let at = studio.addr;
    attach(at, ports).await;

    let seen = until_json(at, PATIENCE, "several status reads with the panic route 404ing", "/api/v1/status", |v| {
        v["devices"][0]["http"]["reads"].as_u64().unwrap_or(0) >= 5
    })
    .await;

    assert!(seen["devices"][0]["panic"].is_null(), "a 404 is silence, not data: {}", seen["devices"][0]);
    assert!(seen["devices"][0]["panic_ago"].is_null(), "{}", seen["devices"][0]);
    assert_eq!(
        seen["devices"][0]["http"]["last_error"],
        serde_json::Value::Null,
        "the status route itself is unaffected by the panic route's 404: {}",
        seen["devices"][0]["http"]
    );
    assert_eq!(seen["ok"], true, "a 404 on the panic route is not a server fault: {}", seen["problems"]);
    assert!(seen["problems"].as_array().expect("problems").is_empty(), "{}", seen["problems"]);
    assert_eq!(server.panic_hits(), 1, "a 404 is asked once, not retried every poll");

    drop(dev);
    studio.stop().await;
    server.stop();
}

// ---------------------------------------------------------------- servers ----

/// A tiny HTTP server that answers a valid status reply, slowly, and counts
/// how many connections are open at once.
struct CountingServer {
    addr: SocketAddr,
    state: Arc<CountState>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct CountState {
    open: AtomicUsize,
    most: AtomicUsize,
    served: AtomicUsize,
    closed: AtomicUsize,
    stop: std::sync::atomic::AtomicBool,
    hosts: std::sync::Mutex<Vec<String>>,
}

impl CountingServer {
    fn start(hold: Duration) -> CountingServer {
        CountingServer::start_at(0, hold).expect("a port")
    }

    /// The same on a **named** port, so a test can take the status API away
    /// and put it back where it was - which is what a panel rebooting looks
    /// like to the studio's poller. Port 0 is an ephemeral one.
    fn start_at(port: u16, hold: Duration) -> std::io::Result<CountingServer> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let addr = listener.local_addr().expect("its address");
        listener.set_nonblocking(true).expect("non-blocking");
        let state = Arc::new(CountState::default());
        let thread = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                let mut workers = Vec::new();
                while !state.stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let state = Arc::clone(&state);
                            workers.push(std::thread::spawn(move || serve_counted(stream, &state, hold)));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
                for w in workers {
                    let _ = w.join();
                }
            })
        };
        Ok(CountingServer { addr, state, thread: Some(thread) })
    }

    /// `(served, most open at once, asked for close, the Host headers seen)`.
    fn report(&self) -> (usize, usize, usize, Vec<String>) {
        (
            self.state.served.load(Ordering::SeqCst),
            self.state.most.load(Ordering::SeqCst),
            self.state.closed.load(Ordering::SeqCst),
            self.state.hosts.lock().expect("the host list").clone(),
        )
    }

    fn stop(mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CountingServer {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve_counted(stream: std::net::TcpStream, state: &Arc<CountState>, hold: Duration) {
    // The socket inherits the listener's non-blocking flag on the BSDs, macOS
    // included; without this the head read below returns WouldBlock at once.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let now = state.open.fetch_add(1, Ordering::SeqCst) + 1;
    state.most.fetch_max(now, Ordering::SeqCst);

    let mut writer = stream.try_clone().expect("a writer");
    let mut reader = BufReader::new(stream);
    let mut close = false;
    let mut host = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim_end().to_string();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("connection:") && lower.contains("close") {
            close = true;
        }
        if let Some(h) = lower.strip_prefix("host:") {
            host = h.trim().to_string();
        }
    }
    // Held open long enough that a second request would certainly overlap.
    std::thread::sleep(hold);
    let body = STATUS_JSON;
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = writer.write_all(head.as_bytes());
    let _ = writer.write_all(body.as_bytes());
    let _ = writer.flush();

    if close {
        state.closed.fetch_add(1, Ordering::SeqCst);
    }
    state.hosts.lock().expect("the host list").push(host);
    state.served.fetch_add(1, Ordering::SeqCst);
    state.open.fetch_sub(1, Ordering::SeqCst);
}

/// `crates/device-api/tests/golden/status.json`, near enough: what a
/// conforming device answers. The SSID is a dummy, as every fixture's is.
const STATUS_JSON: &str = r#"{"api":1,"id":"515151","name":"counted","fw":"0.4.2","boot_id":7,"uptime_ms":1000,
"heap_used":46112,"heap_size":98304,"stack_free":14016,"rssi_dbm":-54,"brightness":96,"idle_mode":"status",
"wifi_state":"connected","ssid":"Example-Wifi1","ip":"127.0.0.1","state":"idle","portal":false,"fw_slot":"ota_0",
"fw_state":"valid","reset_reason":"power_on","store_errors":0}"#;

/// A server that answers a plausible head and then never stops sending.
struct Flood {
    addr: SocketAddr,
    sent: Arc<AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Flood {
    fn start() -> Flood {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let addr = listener.local_addr().expect("its address");
        listener.set_nonblocking(true).expect("non-blocking");
        let sent = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread = {
            let (sent, stop) = (Arc::clone(&sent), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    };
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    let mut scratch = [0u8; 1024];
                    let _ = stream.read(&mut scratch);
                    // No Content-Length: the only end would be the close that
                    // never comes.
                    if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n").is_err() {
                        continue;
                    }
                    let chunk = vec![b'x'; 1024];
                    while !stop.load(Ordering::SeqCst) {
                        if stream.write_all(&chunk).is_err() {
                            break;
                        }
                        sent.fetch_add(chunk.len(), Ordering::SeqCst);
                    }
                }
            })
        };
        Flood { addr, sent, stop, thread: Some(thread) }
    }

    fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Flood {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A tiny HTTP server that answers `GET /api/v1/status` with a **movable**
/// `boot_id` and `GET /api/v1/panic` with a canned reply or a `404`, counting
/// how many times each path was asked for. See the "card 199" comment above
/// for why this exists rather than a real `screeny-sim`.
struct PanicServer {
    addr: SocketAddr,
    state: Arc<PanicState>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct PanicState {
    status_hits: AtomicUsize,
    panic_hits: AtomicUsize,
    boot_id: AtomicU32,
    panic_404: AtomicBool,
    stop: AtomicBool,
}

impl PanicServer {
    fn start(boot_id: u32) -> PanicServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let addr = listener.local_addr().expect("its address");
        listener.set_nonblocking(true).expect("non-blocking");
        let state = Arc::new(PanicState { boot_id: AtomicU32::new(boot_id), ..PanicState::default() });
        let thread = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                let mut workers = Vec::new();
                while !state.stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let state = Arc::clone(&state);
                            workers.push(std::thread::spawn(move || serve_panic(stream, &state)));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
                for w in workers {
                    let _ = w.join();
                }
            })
        };
        PanicServer { addr, state, thread: Some(thread) }
    }

    fn set_boot_id(&self, id: u32) {
        self.state.boot_id.store(id, Ordering::SeqCst);
    }

    fn set_panic_404(&self, on: bool) {
        self.state.panic_404.store(on, Ordering::SeqCst);
    }

    fn status_hits(&self) -> usize {
        self.state.status_hits.load(Ordering::SeqCst)
    }

    fn panic_hits(&self) -> usize {
        self.state.panic_hits.load(Ordering::SeqCst)
    }

    fn stop(mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for PanicServer {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve_panic(stream: std::net::TcpStream, state: &Arc<PanicState>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut writer = stream.try_clone().expect("a writer");
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    // Drain the rest of the head; nothing in it matters here.
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("");

    let (status, reason, body): (u16, &str, String) = if path == screeny_device_api::route::STATUS {
        state.status_hits.fetch_add(1, Ordering::SeqCst);
        (200, "OK", status_json(state.boot_id.load(Ordering::SeqCst)))
    } else if path == screeny_device_api::route::PANIC {
        state.panic_hits.fetch_add(1, Ordering::SeqCst);
        if state.panic_404.load(Ordering::SeqCst) {
            (404, "Not Found", String::new())
        } else {
            (200, "OK", PANIC_JSON.to_string())
        }
    } else {
        (404, "Not Found", String::new())
    };

    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = writer.write_all(head.as_bytes());
    let _ = writer.write_all(body.as_bytes());
    let _ = writer.flush();
}

/// `STATUS_JSON`, near enough, with a `boot_id` a test can move.
fn status_json(boot_id: u32) -> String {
    format!(
        r#"{{"api":1,"id":"515151","name":"panicked","fw":"0.5.2","boot_id":{boot_id},"uptime_ms":1000,
"heap_used":46112,"heap_size":98304,"stack_free":14016,"rssi_dbm":-54,"brightness":96,"idle_mode":"status",
"wifi_state":"connected","ssid":"Example-Wifi1","ip":"127.0.0.1","state":"idle","portal":false,"fw_slot":"ota_0",
"fw_state":"valid","reset_reason":"power_on","store_errors":0}}"#
    )
}

/// `crates/device-api/tests/golden/panic.json`, near enough, but with
/// `consecutive: 2` so the tests exercise the `repeat` flag the chip reads.
const PANIC_JSON: &str = r#"{"boot_count":4,"panic_count":2,"last_panic":{"uptime_ms":94312,"boot":3,
"file":"net.rs","line":321,"consecutive":2},"update":null,"last_reset":"power_on"}"#;
