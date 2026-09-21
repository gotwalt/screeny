//! Card 164: **what a panel costs the network**, measured against a simulator.
//!
//! The counters are only worth having if they are *right*, and "right" here
//! means one thing: the figure on the page has to be the figure a router would
//! bill. So the check is arithmetic that does not go through the counters -
//! frames sent times the mean datagram, plus the 28 bytes of IP and UDP header
//! per datagram - compared with what `/api/v1/status` says.
//!
//! **No test in this file may touch the bench device.** One `screeny-sim` on
//! loopback with explicit ports well above the spec's, discovery off, no state
//! file, and every wait on a deadline.

mod common;

use common::{get, post, until, until_json};
use std::net::SocketAddr;
use std::time::Duration;

use screeny_sim::{Config as SimConfig, SimDevice};
use screeny_studio::{Config, Studio};

/// Long enough for a loaded bench, short enough that a wedged test fails
/// rather than hangs.
const PATIENCE: Duration = Duration::from_secs(30);

/// The rate is an average over `devices::TRAFFIC_WINDOW`, so a figure has to
/// be given a few time constants before it means anything.
const SETTLE: Duration = Duration::from_secs(16);

/// How long the independent measurement is taken over.
const MEASURE: Duration = Duration::from_secs(5);

/// The overhead rule, restated here on purpose: a test that imported the
/// constant it is checking would prove nothing.
const IP_AND_UDP: f64 = 28.0;

#[derive(Clone, Copy, Debug)]
struct Ports {
    frame: u16,
    http: u16,
}

/// A simulator on loopback with known ports. Consecutive frame/control ports,
/// because an `IP:PORT` target takes the control port to be frame + 1.
fn start_sim() -> (SimDevice, Ports) {
    for i in 0..60u16 {
        let ports = Ports { frame: 50_400 + i * 2, http: 50_600 + i };
        let cfg = SimConfig {
            frame_port: ports.frame,
            control_port: ports.frame + 1,
            http: true,
            http_port: ports.http,
            http_port_explicit: true,
            ..SimConfig::for_test()
        };
        if let Ok(dev) = SimDevice::start_with(cfg, None) {
            return (dev, ports);
        }
    }
    panic!("no free simulator ports in 50400..50520 / 50600..50660");
}

/// A studio that reads the simulator's HTTP status often enough for a test to
/// see it happen. The ten-second floor is there for the *device's* one
/// connection worker; a simulator on loopback has a thread per connection, and
/// `main.rs` is what holds the product to the floor.
async fn studio_for(ports: Ports, http_every: Duration) -> screeny_studio::Running {
    let cfg = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        supervise_every: Duration::from_millis(200),
        telemetry_every: Duration::from_millis(500),
        device_http_every: http_every,
        device_http_port: ports.http,
        ..Config::default()
    };
    Studio::bind(cfg).await.expect("bind an ephemeral loopback port").spawn()
}

/// Attach the studio to the simulator and stream at it until `more` further
/// datagrams have gone out. `more` is counted from whatever has already gone,
/// so this is also how a test waits out a *second* link coming up.
async fn attach_and_play_past(at: SocketAddr, ports: Ports, already: u64, more: u64) {
    let body = format!(r#"{{"on":true,"to":"127.0.0.1:{}"}}"#, ports.frame);
    let done = post(at, "/api/v1/set_panel", &body).await;
    assert_eq!(done.status, 200, "{}", String::from_utf8_lossy(&done.body));
    until_json(at, PATIENCE, "the link to come up and carry frames", "/api/v1/status", |v| {
        v["preview"]["panel"]["connected"] == true
            && v["devices"][0]["traffic"]["frames"]["out"]["packets"].as_u64().unwrap_or(0) > already + more
    })
    .await;
}

/// The common case: attach to a panel nothing has been sent to yet.
async fn attach_and_play(at: SocketAddr, ports: Ports) {
    attach_and_play_past(at, ports, 0, 30).await;
}

/// The first device on `/api/v1/status`.
fn device(v: &serde_json::Value) -> &serde_json::Value {
    &v["devices"][0]
}

fn u(v: &serde_json::Value) -> u64 {
    v.as_u64().unwrap_or_else(|| panic!("expected a number, got {v}"))
}

fn f(v: &serde_json::Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("expected a number, got {v}"))
}

/// **The acceptance.** A studio streaming a patch at a panel says how much
/// that costs, and the number agrees with the arithmetic:
/// `frames_sent x mean datagram, plus 28 B a datagram`.
///
/// Everything is read out of `/api/v1/status`, twice, `MEASURE` apart. The
/// independent figure is the difference between the two reads; the figure
/// being checked is the rate the *server* worked out, on its own tick, from
/// its own counters.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rate_out_is_the_frames_times_their_size_plus_the_header() {
    let (sim, ports) = start_sim();
    let studio = studio_for(ports, Duration::from_millis(400)).await;
    let at = studio.addr;
    attach_and_play(at, ports).await;

    // **A patch that holds still**, so "mean frame bytes" is a fact rather
    // than an average over scenes of different sizes: the same pixels for
    // ever encode to the same number of bytes for ever, and that is what makes
    // a rate and an average comparable to within a few per cent at all.
    //
    // It was the test card with its line stopped until card 178 removed it.
    // `metaballs` at `speed` 0 is the same trick on a patch that is still
    // here: its whole picture is a function of `ctx.t * speed`, so with the
    // speed at zero every frame is the frame at t = 0. It reads no clock, so
    // unlike the clock patches it really does hold.
    let done = post(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await;
    assert_eq!(done.status, 200, "{}", String::from_utf8_lossy(&done.body));
    let done = post(at, "/api/v1/set_param", r#"{"id":"speed","value":0.0}"#).await;
    assert_eq!(done.status, 200, "{}", String::from_utf8_lossy(&done.body));

    // The rate is an average with a five-second time constant; give it a few.
    tokio::time::sleep(SETTLE).await;

    let first = get(at, "/api/v1/status").await.json();
    let t0 = std::time::Instant::now();
    tokio::time::sleep(MEASURE).await;
    let second = get(at, "/api/v1/status").await.json();
    let dt = t0.elapsed().as_secs_f64();

    let (a, b) = (device(&first), device(&second));
    let frames_a = &a["traffic"]["frames"]["out"];
    let frames_b = &b["traffic"]["frames"]["out"];

    // Packets are datagrams, and the link's own frame count says how many
    // datagrams there were. They are read at slightly different moments - the
    // counters are rolled up on the supervisor's tick and `frames_sent` is
    // live - so this is "the same number, within a tick's worth of frames"
    // rather than an identity.
    let sent_a = u(&a["player"]["panel"]["frames_sent"]);
    let sent_b = u(&b["player"]["panel"]["frames_sent"]);
    let packets = u(&frames_b["packets"]) - u(&frames_a["packets"]);
    let frames = sent_b - sent_a;
    assert!(
        packets.abs_diff(frames) <= 8,
        "one datagram per frame the link sent: {packets} datagrams against {frames} frames"
    );
    assert!(packets > 60, "the stream should have sent something over {dt:.1} s: {packets}");

    // The mean datagram over the window, which is "mean frame bytes".
    let mean = (u(&frames_b["bytes"]) - u(&frames_a["bytes"])) as f64 / packets as f64;
    assert!(mean > 200.0 && mean < 1472.0, "a frame datagram is {mean:.0} B");

    // **The card's arithmetic**, done here and not through any counter:
    // frames sent x mean frame bytes, plus 28 B a datagram.
    let expected = packets as f64 * (mean + IP_AND_UDP) / dt;
    let reported = f(&b["traffic"]["rate"]["frames_out"]);
    let off = (reported - expected).abs() / expected;
    assert!(
        off <= 0.10,
        "{packets} datagrams of {mean:.0} B in {dt:.2} s is {:.1} KB/s; the page says {:.1} KB/s ({:.1}% out)",
        expected / 1000.0,
        reported / 1000.0,
        off * 100.0
    );
    println!(
        "frames out: {packets} datagrams x {mean:.0} B + {IP_AND_UDP} B in {dt:.2} s = {:.1} KB/s; \
         the page says {:.1} KB/s ({:.1}% out)",
        expected / 1000.0,
        reported / 1000.0,
        off * 100.0
    );

    // `rate.out` is every path together, and frames are nearly all of it.
    let out = f(&b["traffic"]["rate"]["out"]);
    assert!(out >= reported, "the total cannot be less than the frame path: {out} < {reported}");
    assert!(reported / out > 0.9, "frames should be nearly all of it: {reported} of {out}");

    // And the panel talks back: a little telemetry, orders of magnitude less.
    let back = f(&b["traffic"]["rate"]["in"]);
    assert!(back > 0.0, "the panel answers STATS_REQ, and that is traffic too");
    assert!(back < out / 20.0, "in {back:.0} B/s against out {out:.0} B/s");

    // The window is said out loud, so the page can say what it is averaging.
    assert_eq!(f(&b["traffic"]["window_s"]), 5.0);

    drop(studio);
    drop(sim);
}

/// The HTTP path moves **both ways** on each poll, and the control path moves
/// when somebody presses something - which is what makes the split by path
/// worth having rather than one number.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_http_and_control_paths_are_counted_separately() {
    let (sim, ports) = start_sim();
    let studio = studio_for(ports, Duration::from_millis(400)).await;
    let at = studio.addr;
    attach_and_play(at, ports).await;

    // The status poll: bytes written and bytes read, and it keeps happening.
    //
    // Card 199: the studio also asks `GET /api/v1/panic` once, the first time
    // it sees a given `boot_id`, on the same HTTP path - so `http.out.packets`
    // is one more than `http.reads` (which counts *status* reads only) from
    // then on, not equal to it. Waiting for `devices[0].panic` to be present
    // - the simulator's panic route always answers, never `404`s - puts this
    // read safely past that one-time ask, so the extra packet is a fixed `+1`
    // rather than a race between "before" and "after" it landed.
    let first = until_json(at, PATIENCE, "a status read and its one-time panic ask", "/api/v1/status", |v| {
        u(&device(v)["traffic"]["http"]["in"]["packets"]) >= 1 && !device(v)["panic"].is_null()
    })
    .await;
    let http_a = device(&first)["traffic"]["http"].clone();
    assert!(u(&http_a["out"]["bytes"]) > 0, "the request went out: {http_a}");
    assert!(u(&http_a["in"]["bytes"]) > u(&http_a["out"]["bytes"]), "the reply is the bigger half: {http_a}");
    let reads_a = u(&device(&first)["http"]["reads"]);
    assert_eq!(
        u(&http_a["out"]["packets"]),
        reads_a + 1,
        "one request per read that worked, plus the one-time panic ask (card 199): {http_a} vs {reads_a} reads"
    );

    let later = until_json(at, PATIENCE, "a second status read", "/api/v1/status", |v| {
        u(&device(v)["traffic"]["http"]["in"]["packets"]) > u(&http_a["in"]["packets"])
    })
    .await;
    let http_b = device(&later)["traffic"]["http"].clone();
    assert!(u(&http_b["out"]["bytes"]) > u(&http_a["out"]["bytes"]), "both ways, every poll");
    assert!(u(&http_b["in"]["bytes"]) > u(&http_a["in"]["bytes"]));
    // No more panic asks after the first: from here the packet count grows
    // exactly with the number of status reads, one to one.
    let reads_b = u(&device(&later)["http"]["reads"]);
    assert_eq!(
        u(&http_b["out"]["packets"]) - u(&http_a["out"]["packets"]),
        reads_b - reads_a,
        "after the one-time panic ask, packets and reads grow together: {http_a} -> {http_b}"
    );

    // The control path. The telemetry poll is already moving it; a human
    // pressing Identify and moving the brightness slider has to move it too.
    let before = device(&later)["traffic"]["control"].clone();
    let id = device(&later)["id"].as_str().expect("an id").to_string();
    let done = post(at, "/api/v1/device/identify", &format!(r#"{{"device":"{id}","ms":50}}"#)).await;
    assert_eq!(done.status, 200, "{}", String::from_utf8_lossy(&done.body));
    let done = post(at, "/api/v1/device/brightness", &format!(r#"{{"device":"{id}","level":40}}"#)).await;
    assert_eq!(done.status, 200, "{}", String::from_utf8_lossy(&done.body));

    let after = get(at, "/api/v1/status").await.json();
    let ctl = device(&after)["traffic"]["control"].clone();
    assert!(
        u(&ctl["out"]["packets"]) >= u(&before["out"]["packets"]) + 2,
        "two controls are at least two datagrams out: {before} -> {ctl}"
    );
    assert!(
        u(&ctl["in"]["packets"]) >= u(&before["in"]["packets"]) + 2,
        "and the panel replied to both: {before} -> {ctl}"
    );

    // The totals carry the overhead rule: 28 B a datagram for the two UDP
    // paths, nothing for HTTP, where user space cannot see the segments.
    let d = device(&after);
    let t = &d["traffic"];
    let udp = |p: &str, dir: &str| {
        u(&t[p][dir]["bytes"]) as f64 + IP_AND_UDP * u(&t[p][dir]["packets"]) as f64
    };
    let total_out = udp("frames", "out") + udp("control", "out") + u(&t["http"]["out"]["bytes"]) as f64;
    assert!(
        (u(&t["total"]["out"]["bytes"]) as f64 - total_out).abs() < 1.0,
        "the total is the three paths with the overhead added where there is one: {t}"
    );

    drop(studio);
    drop(sim);
}

/// **Totals only grow, and two browsers see one number.**
///
/// The second half is the reason the rate is worked out on the server: a page
/// that divided its own counters would show a different figure in every tab,
/// and the figure would depend on how often that tab happened to poll.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_totals_only_grow_and_every_browser_reads_the_same_rate() {
    let (sim, ports) = start_sim();
    let studio = studio_for(ports, Duration::from_millis(400)).await;
    let at = studio.addr;
    attach_and_play(at, ports).await;

    let read = |v: &serde_json::Value| {
        let t = &device(v)["traffic"];
        [
            u(&t["frames"]["out"]["bytes"]),
            u(&t["frames"]["out"]["packets"]),
            u(&t["frames"]["in"]["bytes"]),
            u(&t["control"]["out"]["bytes"]),
            u(&t["http"]["out"]["bytes"]),
            u(&t["total"]["out"]["bytes"]),
            u(&t["total"]["in"]["bytes"]),
        ]
    };

    let mut was = read(&get(at, "/api/v1/status").await.json());
    for _ in 0..12 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let now = read(&get(at, "/api/v1/status").await.json());
        for (i, (a, b)) in was.iter().zip(now.iter()).enumerate() {
            assert!(b >= a, "counter {i} went backwards: {a} -> {b}");
        }
        was = now;
    }
    assert!(was[1] > 60, "the stream should have sent a good many datagrams: {was:?}");

    // **The panel is taken away and given back**, which rebuilds the link and
    // resets its own counters. The studio's must carry on from where they
    // were - this is the case that makes the counter a *difference* rather
    // than a copy.
    let off = post(at, "/api/v1/set_panel", r#"{"on":false}"#).await;
    assert_eq!(off.status, 200, "{}", String::from_utf8_lossy(&off.body));
    let banked = read(&get(at, "/api/v1/status").await.json());
    attach_and_play_past(at, ports, banked[1], 30).await;
    let again = read(&get(at, "/api/v1/status").await.json());
    for (i, (a, b)) in banked.iter().zip(again.iter()).enumerate() {
        assert!(b >= a, "counter {i} lost what the first link cost: {a} -> {b}");
    }
    assert!(again[1] > banked[1], "and the second link is adding to it: {banked:?} -> {again:?}");

    // Two browsers, one number. The rate changes only on the supervisor's
    // tick, so two reads inside one tick have to be identical - and if a tick
    // did land between them, the totals moved and it is a different moment.
    until(PATIENCE, "two reads inside one tick", || async move {
        let a = get(at, "/api/v1/status").await.json();
        let b = get(at, "/api/v1/status").await.json();
        let (ta, tb) = (&device(&a)["traffic"], &device(&b)["traffic"]);
        if ta["total"]["out"]["bytes"] != tb["total"]["out"]["bytes"] {
            return false; // a tick landed between them; try again
        }
        assert_eq!(ta["rate"], tb["rate"], "two browsers must not see two rates");
        assert_eq!(ta["frames"], tb["frames"]);
        true
    })
    .await;

    drop(studio);
    drop(sim);
}

/// A panel the studio has never reached costs nothing on the frame path, and
/// its rate stays at nothing - which is what stops the page telling somebody
/// that a panel in a drawer is using their network.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panel_that_is_away_sends_no_frames() {
    let studio = Studio::bind(Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        supervise_every: Duration::from_millis(200),
        telemetry_every: Duration::from_millis(200),
        ..Config::default()
    })
    .await
    .expect("bind an ephemeral loopback port")
    .spawn();
    let at = studio.addr;

    // Port 1 on loopback: nothing is listening and nothing can be.
    let added = post(at, "/api/v1/devices/add", r#"{"to":"127.0.0.1:1","name":"in a drawer"}"#).await;
    assert_eq!(added.status, 200, "{}", String::from_utf8_lossy(&added.body));

    tokio::time::sleep(Duration::from_secs(2)).await;
    let v = get(at, "/api/v1/status").await.json();
    let t = &device(&v)["traffic"];
    assert_eq!(u(&t["frames"]["out"]["packets"]), 0, "nothing was streamed at it: {t}");
    assert_eq!(u(&t["frames"]["in"]["packets"]), 0, "and it said nothing back: {t}");
    assert_eq!(u(&t["http"]["in"]["packets"]), 0, "there is nowhere to read its status from: {t}");
    assert_eq!(f(&t["rate"]["frames_out"]), 0.0, "and the page says 0 KB/s: {t}");

    drop(studio);
}
