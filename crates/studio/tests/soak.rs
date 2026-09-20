//! The soak: bounded, automated, and at accelerated time.
//!
//! The card asks for hours of wall clock on `workbench.local` after card 107,
//! watched through `/healthz`. This is the other half of that: a test that
//! makes the same things go wrong in a minute rather than in a week, and
//! asserts the three properties a thing meant to be forgotten must have -
//! **flat memory**, **nothing dies**, and **it recovers from every fault**.
//!
//! Bounded by construction, because a soak that ran away would be exactly the
//! sin it is meant to catch: a fixed number of rounds, a deadline on every
//! wait, a simulator that exits by itself, and a total that is measured in
//! seconds. `SCREENY_SOAK_SECS` lengthens it for the one long run on the final
//! build; the default is short enough to live in `cargo test`.
//!
//! No LAN. Loopback, a port band well above the spec's, mDNS off, discovery
//! off.

mod common;

use common::{get, post, studio_in, until_json, Temp};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use screeny_sim::{Config as SimConfig, Faults, SimDevice};

const FIRST_PORT: u16 = 50_920;
const LAST_PORT: u16 = 50_948;
/// The panel's own id, which never changes however often it moves or reboots.
const DEVICE: &str = "50ak01";

fn sim_on(port: u16) -> Option<SimDevice> {
    SimDevice::start(SimConfig {
        frame_port: port,
        control_port: port + 1,
        id: DEVICE.to_string(),
        instance: format!("sim-{DEVICE}"),
        name: String::new(),
        ..SimConfig::for_test()
    })
    .ok()
}

/// A free pair at or after `from`, wrapping round the band - a long soak moves
/// the panel dozens of times and must not run off the end of it. The port it
/// has just left is free again, so wrapping always finds one.
fn start_sim(from: u16) -> (SimDevice, u16) {
    let start = if from >= LAST_PORT { FIRST_PORT } else { from };
    for port in (start..LAST_PORT).step_by(2).chain((FIRST_PORT..start).step_by(2)) {
        if let Some(dev) = sim_on(port) {
            return (dev, port);
        }
    }
    panic!("no free consecutive port pair in {FIRST_PORT}..{LAST_PORT}");
}

/// **The panel comes back on the address it left**, which is the whole point
/// of round 2, so this one may not fall back to another port.
///
/// The pair it has just released is normally free at once; a second copy of
/// this suite in another worktree can hold it for a moment (card 117), so this
/// waits rather than failing on the first refusal - and says which port if it
/// never comes free.
async fn sim_again(port: u16) -> SimDevice {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(dev) = sim_on(port) {
            return dev;
        }
        assert!(
            Instant::now() < deadline,
            "the panel could not come back on 127.0.0.1:{port}/{}: the pair never came free",
            port + 1
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Resident set size in KiB, from the one tool every Unix has. Good enough:
/// what is being looked for is a leak, which shows up as a slope, not as a
/// byte.
fn rss_kib() -> u64 {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output();
    out.ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok()).unwrap_or(0)
}

async fn status(at: SocketAddr) -> serde_json::Value {
    get(at, "/api/v1/status").await.json()
}

async fn device(at: SocketAddr) -> serde_json::Value {
    status(at).await["devices"].as_array().and_then(|d| d.first().cloned()).unwrap_or_default()
}

async fn frames_sent(at: SocketAddr) -> u64 {
    device(at).await["player"]["panel"]["frames_sent"].as_u64().unwrap_or(0)
}

/// How long any one wait here may take.
///
/// Generous on purpose, and deliberately not a measure of anything: this test
/// is run beside release builds and other suites, and a machine that is busy
/// should make the soak *slow*, not red (card 093). Every wait that ends here
/// says what it was waiting for and what the numbers were when it gave up.
const WAIT: Duration = Duration::from_secs(30);

/// Frames that have to reach the wire before frames count as flowing again.
/// A third of a second at 30 fps.
const FLOWING: u64 = 10;

/// How old the last telemetry may be for the poll to count as alive, in
/// seconds. The poll runs every 200 ms here, so this is **fifty periods**: it
/// asks whether the task is running at all, not whether it was prompt.
const FRESH: f64 = 10.0;

/// Recovery, defined once: the link is up again and frames are moving again.
///
/// Returns how many times the frame counter had to be re-based; see
/// [`flowing`].
async fn recovered(at: SocketAddr, what: &str) -> u32 {
    until_json(at, WAIT, &format!("the link to come back after {what}"), "/api/v1/status", |v| {
        v["devices"][0]["player"]["panel"]["connected"] == true
    })
    .await;
    let rebased = flowing(at, what).await;
    let h = get(at, "/healthz").await;
    assert_eq!(h.status, 200, "/healthz after {what}: {}", String::from_utf8_lossy(&h.body));
    rebased
}

/// Wait until [`FLOWING`] more frames have reached the wire.
///
/// **The counter can go backwards, and that is the whole reason this is not
/// two lines.** `panel.frames_sent` is the *link's* lifetime count, and the
/// supervisor builds a new link whenever the way to reach the panel changes: a
/// panel that moved to another address (round 3), or one whose resolution went
/// stale while it was away. The new link starts at zero, so a plain
/// `now > before + FLOWING` waits for the new link to count its way past the
/// old link's total - at 30 fps, a second per thirty frames, which half a
/// minute into a soak is longer than any patience worth having. It is also
/// exactly the shape of a flake that only bites on a loaded machine, where the
/// earlier rounds got further before this one started.
///
/// So the property is "the count is going up *from wherever it is now*", and a
/// count that drops re-bases it. Returns how many times it did, which the
/// summary prints: a soak that never re-based has not exercised a link rebuild.
async fn flowing(at: SocketAddr, what: &str) -> u32 {
    let start = Instant::now();
    let deadline = start + WAIT;
    let mut base = frames_sent(at).await;
    let mut rebased = 0u32;
    loop {
        let now = frames_sent(at).await;
        if now < base {
            // Once per rebuild, which is at most once per round: this cannot
            // become a line per event.
            println!(
                "soak: after {what} the panel's frame counter went backwards, {base} -> {now}: \
                 the link was rebuilt, so frames flowing is counted from here. \
                 (Waiting for {} would have meant another {:.0} s at 30 fps, and that is the flake card 117 is about.)",
                base + FLOWING,
                (base + FLOWING - now) as f64 / 30.0
            );
            base = now;
            rebased += 1;
        }
        if now >= base + FLOWING {
            return rebased;
        }
        assert!(
            Instant::now() < deadline,
            "after {what}: {FLOWING} frames did not reach the wire in {:.1} s. \
             frames_sent {now}, counting from {base} ({rebased} link rebuilds under the counter). The panel: {}",
            start.elapsed().as_secs_f64(),
            device(at).await["player"]["panel"]
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_survives_a_bounded_soak() {
    let want = std::env::var("SCREENY_SOAK_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(60);
    let deadline = Instant::now() + Duration::from_secs(want);

    let dir = Temp::new("soak");
    let (mut sim, mut port) = start_sim(FIRST_PORT);
    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;

    post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{port}","name":"soak","play":true}}"#)).await;
    // The typed address is provisional until the panel says who it is, and
    // only then is there a device id to configure a player against.
    until_json(at, WAIT, "the soak panel to say who it is", "/api/v1/status", |v| v["devices"][0]["id"] == DEVICE).await;
    let set = post(at, "/api/v1/player/set", &format!(r#"{{"device":"{DEVICE}","piece":"plasma","seed":1,"fps":30}}"#)).await;
    assert_eq!(set.status, 200, "{}", String::from_utf8_lossy(&set.body));
    until_json(at, WAIT, "the soak panel to start playing", "/api/v1/status", |v| {
        v["devices"][0]["player"]["panel"]["connected"] == true
    })
    .await;

    // Let it settle before the baseline: the first seconds allocate the
    // piece, the pipeline, the encoder's tables and the link's buffers, and
    // none of that is a leak.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let base_rss = rss_kib();
    let base_ticks = device(at).await["player"]["health"]["ticks"].as_u64().unwrap_or(0);
    let started = Instant::now();
    let mut faults = 0u32;
    let mut round = 0u32;
    // How often the panel's frame counter went backwards under us; see
    // `flowing`. Printed, because it is the difference between "this soak
    // exercised a link rebuild" and "it happened not to".
    let mut rebased = 0u32;

    while Instant::now() < deadline {
        round += 1;
        match round % 4 {
            // 1. A quarter of the frames never arrive. The link should not
            //    care: UDP loses frames and the next one is along in 33 ms.
            1 => {
                sim.handle().set_faults(Faults { drop_pct: 25.0, ..Faults::default() });
                tokio::time::sleep(Duration::from_secs(3)).await;
                sim.handle().set_faults(Faults::default());
                rebased += recovered(at, "a quarter of the frames being dropped").await;
            }
            // 2. The panel is unplugged for longer than the silence watchdog
            //    and comes back on the same address.
            2 => {
                drop(sim);
                tokio::time::sleep(Duration::from_secs(7)).await;
                let h = get(at, "/healthz").await;
                assert_eq!(
                    h.status,
                    200,
                    "round {round}: a panel that is away must not make the server unhealthy: {}",
                    String::from_utf8_lossy(&h.body)
                );
                sim = sim_again(port).await;
                rebased += recovered(at, "the panel going away and coming back").await;
            }
            // 3. The panel moves. An address is a way of reaching a panel and
            //    not a name for it, so this one needs telling - but the device
            //    keeps its id, and therefore its player and its piece.
            3 => {
                drop(sim);
                tokio::time::sleep(Duration::from_millis(500)).await;
                let (new_sim, new_port) = start_sim(port + 2);
                sim = new_sim;
                port = new_port;
                let moved = post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{port}","device":"{DEVICE}"}}"#)).await;
                assert_eq!(moved.status, 200, "{}", String::from_utf8_lossy(&moved.body));
                until_json(at, WAIT, &format!("the moved panel to be found again at 127.0.0.1:{port}"), "/api/v1/status", |v| {
                    let d = &v["devices"][0];
                    d["resolved"] == true && d["frame_addr"] == format!("127.0.0.1:{port}")
                })
                .await;
                rebased += recovered(at, "the panel moving to another address").await;
                let d = device(at).await;
                assert_eq!(d["id"], DEVICE, "a panel that moved is still the same panel: {d}");
                assert_eq!(d["player"]["piece"], "plasma", "a panel that moved is still playing the same thing: {d}");
            }
            // 4. Everything the operator does that is not a fault: change the
            //    piece, the seed and the rate, over and over. This is where a
            //    leak in the render core or the state store would show.
            _ => {
                for (piece, fps) in [("metaballs", 60), ("plasma", 30), ("testcard", 10)] {
                    post(at, "/api/v1/player/set", &format!(r#"{{"device":"{DEVICE}","piece":"{piece}","fps":{fps}}}"#)).await;
                    post(at, "/api/v1/set_piece", &format!(r#"{{"id":"{piece}"}}"#)).await;
                    post(at, "/api/v1/set_seed", r#"{"seed":null}"#).await;
                    tokio::time::sleep(Duration::from_millis(700)).await;
                }
                post(at, "/api/v1/player/set", &format!(r#"{{"device":"{DEVICE}","piece":"plasma","fps":30}}"#)).await;
                rebased += recovered(at, "a run of changes").await;
            }
        }
        faults += 1;
        let h = get(at, "/healthz").await;
        assert_eq!(
            h.status,
            200,
            "/healthz at the end of round {round}, {:.0} s in: {}",
            started.elapsed().as_secs_f64(),
            String::from_utf8_lossy(&h.body)
        );
    }

    // ---- what it all came to ----
    //
    // **Wait for the telemetry poll to be current before reading the end.**
    // The last round can take the panel away seconds before the deadline -
    // round 2 unplugs it for seven - and the poll being behind at that instant
    // is the panel's absence, not a dead task. Seen on 2026-09-20: a run that
    // ended one round after the panel came back read `telemetry_ago` at exactly
    // 10.0 s with `asking for telemetry: Connection refused` beside it, and
    // failed an assertion about a thread that was perfectly alive.
    //
    // What says the task is alive is that it *catches up*, which is a bounded
    // wait like every other one here - and the answer that satisfies it is the
    // single moment everything below is asserted on (`common::until_json`).
    let end = until_json(at, WAIT, "the telemetry poll to catch up after the last round", "/api/v1/status", |v| {
        v["devices"][0]["telemetry_ago"].as_f64().unwrap_or(999.0) < FRESH
    })
    .await;
    let d = end["devices"].as_array().and_then(|x| x.first().cloned()).expect("the panel");
    let ticks = d["player"]["health"]["ticks"].as_u64().expect("ticks");
    let end_rss = rss_kib();
    let grew = end_rss.saturating_sub(base_rss);
    let elapsed = want;

    println!(
        "soak: {elapsed} s, {faults} faults in {round} rounds, {rebased} link rebuilds under the frame counter\n\
         soak: rss {base_rss} -> {end_rss} KiB ({:+} KiB, {:+.1}%)\n\
         soak: rendered {} frames ({} since the baseline), {} sent to the panel, {} reconnects\n\
         soak: panics {}, stalls {}, restarts {}, state written {} times, telemetry {} s old",
        grew as i64,
        100.0 * grew as f64 / base_rss.max(1) as f64,
        ticks,
        ticks - base_ticks,
        d["player"]["panel"]["frames_sent"],
        // Card 171: the player-lifetime count, which survives a link rebuild.
        d["player"]["health"]["reconnects"].as_u64().unwrap_or(0),
        d["player"]["health"]["panics"],
        d["player"]["health"]["stalls"],
        d["player"]["health"]["restarts"],
        end["state"]["writes"],
        d["telemetry_ago"],
    );

    // Nothing died. Each of these is a different thread or task: the render
    // loop, the telemetry poll, the state writer, the preview engine.
    //
    // **Every one of these numbers is in its own failure message.** The card
    // this comes from exists because `assert!(x > y)` with nothing beside it
    // once failed on a loaded bench and left nothing to go on but the fact
    // that it had. None of them is a rate a busy machine can miss: they are
    // "did this thread run at all" thresholds, a hundredth of what a working
    // studio does in the same time.
    assert!(
        ticks > base_ticks + 100,
        "the render loop stopped: {ticks} ticks at the end against {base_ticks} at the baseline, \
         {} in {elapsed} s (a 30 fps player does that in four seconds)",
        ticks - base_ticks
    );
    assert_eq!(d["player"]["running"], true, "the render thread is gone: {}", d["player"]["health"]);
    // True by construction - it is what the wait above waited for - and stated
    // anyway, because it is one of the three properties this test is for.
    let telemetry_ago = d["telemetry_ago"].as_f64().unwrap_or(999.0);
    assert!(
        telemetry_ago < FRESH,
        "the telemetry poll stopped: the last telemetry is {telemetry_ago:.1} s old after {elapsed} s, \
         and the poll period is 0.2 s: {d}"
    );
    let writes = end["state"]["writes"].as_u64().unwrap_or(0);
    assert!(writes > 0, "the state writer stopped: {writes} writes in {elapsed} s: {}", end["state"]);
    assert_eq!(end["state"]["last_error"], serde_json::Value::Null, "{}", end["state"]);
    assert_eq!(end["preview"]["alive"], true, "the preview engine is gone: {}", end["preview"]);
    assert_eq!(end["preview"]["wedged"], false, "{}", end["preview"]);
    assert_eq!(end["ok"], true, "{}", end["problems"]);
    assert_eq!(d["player"]["health"]["panics"], 0, "nothing should have panicked: {}", d["player"]["health"]);
    assert_eq!(d["player"]["health"]["stalls"], 0, "nothing should have stalled: {}", d["player"]["health"]);
    assert!(
        faults >= 3,
        "the soak should have injected at least three faults, not {faults} in {round} rounds over {elapsed} s \
         (each round is a fault, and the shortest is three seconds)"
    );

    // Flat memory. Generous, because this measures the whole test process -
    // the simulator, the test's own HTTP client and the studio together - and
    // because a leak is a slope, not a byte. A player that leaked one frame
    // per tick would be tens of megabytes by here.
    assert!(
        grew < 40_000 && (base_rss == 0 || grew * 100 / base_rss.max(1) < 60),
        "memory grew by {grew} KiB over {elapsed} s, from {base_rss} to {end_rss}"
    );

    studio.stop().await;
}
