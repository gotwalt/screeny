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

use common::{get, post, studio_in, until, Temp};
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

/// Recovery, defined once: the link is up again and frames are moving again.
async fn recovered(at: SocketAddr, what: &str) {
    let before = frames_sent(at).await;
    until(Duration::from_secs(30), &format!("the link to come back after {what}"), || async {
        device(at).await["player"]["panel"]["connected"] == true
    })
    .await;
    until(Duration::from_secs(30), &format!("frames to flow again after {what}"), || async {
        frames_sent(at).await > before + 10
    })
    .await;
    let h = get(at, "/healthz").await;
    assert_eq!(h.status, 200, "after {what}: {}", String::from_utf8_lossy(&h.body));
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
    until(Duration::from_secs(30), "the soak panel to say who it is", || async { device(at).await["id"] == DEVICE }).await;
    let set = post(at, "/api/v1/player/set", &format!(r#"{{"device":"{DEVICE}","piece":"plasma","seed":1,"fps":30}}"#)).await;
    assert_eq!(set.status, 200, "{}", String::from_utf8_lossy(&set.body));
    until(Duration::from_secs(30), "the soak panel to start playing", || async {
        device(at).await["player"]["panel"]["connected"] == true
    })
    .await;

    // Let it settle before the baseline: the first seconds allocate the
    // piece, the pipeline, the encoder's tables and the link's buffers, and
    // none of that is a leak.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let base_rss = rss_kib();
    let base_ticks = device(at).await["player"]["health"]["ticks"].as_u64().unwrap_or(0);
    let mut faults = 0u32;
    let mut round = 0u32;

    while Instant::now() < deadline {
        round += 1;
        match round % 4 {
            // 1. A quarter of the frames never arrive. The link should not
            //    care: UDP loses frames and the next one is along in 33 ms.
            1 => {
                sim.handle().set_faults(Faults { drop_pct: 25.0, ..Faults::default() });
                tokio::time::sleep(Duration::from_secs(3)).await;
                sim.handle().set_faults(Faults::default());
                recovered(at, "a quarter of the frames being dropped").await;
            }
            // 2. The panel is unplugged for longer than the silence watchdog
            //    and comes back on the same address.
            2 => {
                drop(sim);
                tokio::time::sleep(Duration::from_secs(7)).await;
                assert_eq!(get(at, "/healthz").await.status, 200, "a panel that is away must not make the server unhealthy");
                sim = sim_on(port).expect("the panel comes back on the same address");
                recovered(at, "the panel going away and coming back").await;
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
                until(Duration::from_secs(30), "the moved panel to be found again", || async {
                    let d = device(at).await;
                    d["resolved"] == true && d["frame_addr"] == format!("127.0.0.1:{port}")
                })
                .await;
                recovered(at, "the panel moving to another address").await;
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
                recovered(at, "a run of changes").await;
            }
        }
        faults += 1;
        assert_eq!(get(at, "/healthz").await.status, 200, "round {round}");
    }

    // ---- what it all came to ----
    let end = status(at).await;
    let d = end["devices"].as_array().and_then(|x| x.first().cloned()).expect("the panel");
    let ticks = d["player"]["health"]["ticks"].as_u64().expect("ticks");
    let end_rss = rss_kib();
    let grew = end_rss.saturating_sub(base_rss);
    let elapsed = want;

    println!(
        "soak: {elapsed} s, {faults} faults in {round} rounds\n\
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
    assert!(ticks > base_ticks + 100, "the render loop stopped: {ticks} vs {base_ticks}");
    assert_eq!(d["player"]["running"], true, "the render thread is gone");
    assert!(d["telemetry_ago"].as_f64().unwrap_or(999.0) < 10.0, "the telemetry poll stopped: {d}");
    assert!(end["state"]["writes"].as_u64().unwrap_or(0) > 0, "the state writer stopped");
    assert_eq!(end["state"]["last_error"], serde_json::Value::Null);
    assert_eq!(end["preview"]["alive"], true, "the preview engine is gone");
    assert_eq!(end["preview"]["wedged"], false);
    assert_eq!(end["ok"], true, "{}", end["problems"]);
    assert_eq!(d["player"]["health"]["panics"], 0, "nothing should have panicked");
    assert_eq!(d["player"]["health"]["stalls"], 0, "nothing should have stalled");
    assert!(faults >= 3, "the soak should have injected at least three faults, not {faults}");

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
