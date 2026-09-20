//! Card 106's acceptance: devices, players, state and containment, against
//! `screeny-sim` on loopback.
//!
//! **No test here may touch the bench device.** Every target is an explicit
//! `127.0.0.1`, discovery is off in every studio these tests start, and the
//! ports are a fixed band well above the spec's so a stray packet could not
//! reach the real panel even in principle.
//!
//! The one that matters is
//! [`the_panel_comes_back_whatever_is_restarted`]: kill the simulator, the
//! server, or both, in any order, and the panel comes back showing what it was
//! showing with nobody doing anything.

mod common;

use common::{get, post, studio_in, until, until_json, Temp, PATIENCE};
use std::net::SocketAddr;
use std::time::Duration;

use screeny_sim::{Config as SimConfig, SimDevice};

/// The port band these tests use. Well above 49374/49375.
const FIRST_PORT: u16 = 50_800;
const LAST_PORT: u16 = 50_900;

/// A simulator on **consecutive** loopback ports, mDNS off.
///
/// Consecutive because an `IP:PORT` a human types resolves with the control
/// port taken to be frame + 1 - true of the spec's defaults, never true of an
/// ephemeral pair. Card 101 hit this first; this is the same workaround.
fn start_sim(id: &str) -> (SimDevice, u16) {
    for port in FIRST_PORT..LAST_PORT {
        if let Some(dev) = sim_on(port, id) {
            return (dev, port);
        }
    }
    panic!("no free consecutive port pair in {FIRST_PORT}..{LAST_PORT}");
}

/// The same simulator again, on a port that is already known - which is how a
/// panel that has been power-cycled comes back.
fn sim_on(port: u16, id: &str) -> Option<SimDevice> {
    let cfg = SimConfig {
        frame_port: port,
        control_port: port + 1,
        id: id.to_string(),
        instance: format!("sim-{id}"),
        name: String::new(),
        // A cap below 255, so "never raise brightness above the device's cap"
        // is something a test can actually see.
        brightness_cap: 120,
        ..SimConfig::for_test()
    };
    SimDevice::start(cfg).ok()
}

/// Add a panel by address and start playing on it, the way the dashboard does.
async fn add_and_play(at: SocketAddr, port: u16) -> String {
    let added = post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{port}","name":"desk","play":true}}"#)).await;
    assert_eq!(added.status, 200, "{}", String::from_utf8_lossy(&added.body));
    added.json()["id"].as_str().expect("an id").to_string()
}

/// The first device in a `/api/v1/status` answer.
fn device_of(status: &serde_json::Value) -> serde_json::Value {
    status["devices"].as_array().and_then(|d| d.first().cloned()).unwrap_or(serde_json::Value::Null)
}

/// The device in `/api/v1/status`, whatever its id has become.
async fn device_status(at: SocketAddr) -> serde_json::Value {
    device_of(&get(at, "/api/v1/status").await.json())
}

/// Wait until the studio has got the device's own id out of it and the player
/// is streaming, and hand back **that** answer.
///
/// Returning it matters on a loaded host: a test that waits and then re-reads
/// is asserting about a different moment from the one it waited for.
async fn wait_until_streaming(at: SocketAddr, what: &str) -> serde_json::Value {
    until_json(at, Duration::from_secs(30), what, "/api/v1/status", |s| {
        let d = device_of(s);
        d["resolved"] == true
            && d["player"]["panel"]["connected"] == true
            && d["player"]["panel"]["frames_sent"].as_u64().unwrap_or(0) > 5
    })
    .await
}

/// A manually typed address becomes a device keyed by the panel's own id, and
/// the player starts playing on it - the whole path, over HTTP.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_typed_address_becomes_a_device_and_starts_playing() {
    let dir = Temp::new("typed");
    let (_sim, port) = start_sim("aa11bb");
    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;

    let provisional = add_and_play(at, port).await;
    assert!(provisional.starts_with("pending:"), "{provisional}");

    // The telemetry poll is periodic and is not ordered against the first
    // frame, so this waits for both rather than assuming that starting to
    // stream means a device has already been asked how it is.
    let s = until_json(
        at,
        Duration::from_secs(30),
        "the typed address to resolve, start playing and be heard from",
        "/api/v1/status",
        |s| {
            let d = device_of(s);
            d["resolved"] == true
                && d["player"]["panel"]["connected"] == true
                && d["player"]["panel"]["frames_sent"].as_u64().unwrap_or(0) > 5
                && d["telemetry"]["state"].is_string()
        },
    )
    .await;

    let d = device_of(&s);
    assert_eq!(d["id"], "aa11bb", "the device should be keyed by its own id, not by its address: {d}");
    assert_eq!(d["label"], "desk", "the name typed in should survive adopting the real id");
    assert_eq!(d["manual"], true);
    assert_eq!(d["control_addr"], format!("127.0.0.1:{}", port + 1));
    assert_eq!(d["player"]["running"], true, "{d}");
    // Card 170: the first panel a studio meets becomes the one the page is a
    // window onto, with nobody choosing anything.
    assert_eq!(d["attached"], true, "the panel the page is showing: {d}");
    assert_eq!(d["player"]["focused"], true);
    assert_eq!(s["preview"]["device"], "aa11bb", "{}", s["preview"]);
    assert_eq!(s["ok"], true, "{}", s["problems"]);

    // And `/healthz` is happy with all of it.
    let h = get(at, "/healthz").await;
    assert_eq!(h.status, 200, "{}", String::from_utf8_lossy(&h.body));
    println!(
        "typed address: pending -> {}, {} frames sent, telemetry state {}, rssi {}",
        d["id"], d["player"]["panel"]["frames_sent"], d["telemetry"]["state"], d["telemetry"]["rssi_dbm"]
    );
}

/// **Card 170's acceptance, in one test**: the page and the panel are one
/// picture. Changing the piece through the design view's own route changes
/// what the panel is playing, at once and with nothing promoted.
///
/// This replaces `promoting_the_preview_is_explicit`, which pinned the
/// opposite - that playing with the design view did *not* touch the panel.
/// That was card 106's reading of the vision and it is the thing the owner
/// corrected, so the test is inverted rather than deleted: the behaviour it
/// described must not come back.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn the_page_and_the_panel_are_one() {
    let dir = Temp::new("onepicture");
    let (_sim, port) = start_sim("aa44aa");
    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    add_and_play(at, port).await;
    wait_until_streaming(at, "the player to start").await;

    // The design view's routes, unchanged since card 105 - and now they are
    // the panel's.
    post(at, "/api/v1/set_piece", r#"{"id":"metaballs"}"#).await;
    post(at, "/api/v1/set_seed", r#"{"seed":777}"#).await;
    let s = until_json(at, PATIENCE, "the panel to follow the page", "/api/v1/status", |s| {
        let d = device_of(s);
        d["player"]["piece"] == "metaballs" && d["player"]["seed"] == 777
    })
    .await;
    assert_eq!(s["preview"]["piece"], "metaballs", "and the page says the same thing: {}", s["preview"]);
    assert_eq!(s["preview"]["seed"], 777);

    // A parameter, too - the half card 166 was written for.
    //
    // **Both halves out of one read, and waited for rather than sampled.**
    // "It took the parameter" and "it is still streaming" are asserted on the
    // same answer (`until_json` hands back the one that satisfied it), and
    // being connected is waited for: the supervisor rebuilds the link when the
    // studio learns where the panel really is - a typed address becoming a
    // resolved device - and a loaded machine can put that rebuild exactly
    // here. Card 117: this line was `assert_eq!(..., true)` with no message
    // and no wait, and it failed once, under a full loaded suite, saying
    // nothing.
    post(at, "/api/v1/set_param", r#"{"id":"count","value":8.0}"#).await;
    let s = until_json(
        at,
        Duration::from_secs(30),
        "the panel to take the parameter and be streaming it",
        "/api/v1/status",
        |s| {
            let d = device_of(s);
            d["player"]["params"]["count"] == 8.0 && d["player"]["panel"]["connected"] == true
        },
    )
    .await;
    // The page's own state carries every parameter at its effective value, so
    // a browser can draw the sliders from it.
    let boot = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(boot["state"]["params"]["count"], 8.0);
    assert!(boot["state"]["params"].as_object().expect("params").len() > 1, "{}", boot["state"]["params"]);
    assert_eq!(boot["state"]["device"], "aa44aa", "the page says which panel it is a window onto");

    // And it is still streaming it, not just configured to - said of the same
    // moment the parameter was read from, with the link's own account of
    // itself if it is not.
    assert_eq!(
        device_of(&s)["player"]["panel"]["connected"],
        true,
        "the panel is streamed to, not merely configured: {}",
        device_of(&s)["player"]["panel"]
    );
    // Nothing is left that could promote a second picture onto the panel.
    let gone = post(at, "/api/v1/player/adopt_preview", r#"{"device":"aa44aa"}"#).await;
    assert_eq!(gone.status, 404, "`adopt_preview` should be gone, not quietly doing something");
    studio.stop().await;
}

/// **The card's acceptance.** Kill the simulator, the server, or both, in any
/// order: the panel comes back showing what it was showing, with no operator
/// action, every time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_panel_comes_back_whatever_is_restarted() {
    let dir = Temp::new("restart");
    let (mut sim, port) = start_sim("cc22dd");
    let mut studio = studio_in(&dir.0, false).await;
    let mut at = studio.addr;

    add_and_play(at, port).await;
    wait_until_streaming(at, "the first run to start playing").await;

    // Something specific, so "showing what it was showing" is checkable.
    let set = post(at, "/api/v1/player/set", r#"{"device":"cc22dd","piece":"metaballs","seed":4242,"fps":30}"#).await;
    assert_eq!(set.status, 200, "{}", String::from_utf8_lossy(&set.body));
    wait_until_streaming(at, "the chosen piece to start playing").await;

    for (round, what) in [(1, "the simulator"), (2, "the server"), (3, "both, server first"), (4, "both, panel first")] {
        match round {
            1 => {
                drop(sim);
                tokio::time::sleep(Duration::from_millis(400)).await;
                sim = sim_on(port, "cc22dd").expect("the panel comes back on the same port");
            }
            2 => {
                studio.stop().await;
                studio = studio_in(&dir.0, false).await;
                at = studio.addr;
            }
            3 => {
                studio.stop().await;
                drop(sim);
                tokio::time::sleep(Duration::from_millis(400)).await;
                sim = sim_on(port, "cc22dd").expect("the panel comes back");
                studio = studio_in(&dir.0, false).await;
                at = studio.addr;
            }
            _ => {
                drop(sim);
                studio.stop().await;
                tokio::time::sleep(Duration::from_millis(400)).await;
                studio = studio_in(&dir.0, false).await;
                at = studio.addr;
                sim = sim_on(port, "cc22dd").expect("the panel comes back");
            }
        }

        // One read: what it waited for and what it asserts are the same moment.
        let s = wait_until_streaming(at, &format!("the panel to come back after restarting {what}")).await;
        let d = device_of(&s);
        assert_eq!(d["id"], "cc22dd", "after restarting {what}: {d}");
        assert_eq!(d["player"]["piece"], "metaballs", "after restarting {what}, it is playing something else: {d}");
        assert_eq!(d["player"]["seed"], 4242, "after restarting {what}, the seed changed: {d}");
        assert_eq!(d["player"]["fps"], 30.0, "after restarting {what}, the rate changed: {d}");
        println!(
            "after restarting {what}: {} playing {} seed {}, {} frames sent, link {}",
            d["id"], d["player"]["piece"], d["player"]["seed"], d["player"]["panel"]["frames_sent"], d["player"]["panel"]["state"]
        );
    }
    studio.stop().await;
}

/// The page survives a restart too - including the two things card 105
/// deliberately left out of its state and card 106 put in: whether a panel is
/// being driven, and which one. Since card 170 those are the player's own
/// `on` and `device`, and the playback state travels with them.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn the_page_resumes_where_it_was() {
    let dir = Temp::new("preview");
    let (_sim, port) = start_sim("ee33ff");
    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;

    post(at, "/api/v1/set_piece", r#"{"id":"plasma"}"#).await;
    post(at, "/api/v1/set_seed", r#"{"seed":1234}"#).await;
    post(at, "/api/v1/set_playback", r#"{"paused":true,"speed":2.0,"fps":30.0}"#).await;
    let panel = post(at, "/api/v1/set_panel", &format!(r#"{{"on":true,"to":"127.0.0.1:{port}"}}"#)).await;
    assert_eq!(panel.status, 200);
    studio.stop().await;

    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    let state = get(at, "/api/v1/bootstrap").await.json()["state"].clone();
    assert_eq!(state["piece"], "plasma");
    assert_eq!(state["seed"], 1234);
    assert_eq!(state["paused"], true);
    assert_eq!(state["speed"], 2.0);
    assert_eq!(state["fps"], 30.0);

    // Which panel it was attached to, and that it is driving it again - all
    // out of one answer, so nothing here is about two different moments.
    let s = until_json(at, Duration::from_secs(30), "the panel link to come back up", "/api/v1/status", |s| {
        s["preview"]["panel"]["connected"] == true
    })
    .await;
    assert_eq!(s["preview"]["piece"], "plasma", "{}", s["preview"]);
    assert_eq!(s["preview"]["panel_on"], true, "it should still be driving a panel: {}", s["preview"]);
    assert_eq!(s["preview"]["panel_to"], format!("127.0.0.1:{port}"));
    assert_eq!(s["preview"]["paused"], true, "including the playback state, which used to be the preview's");
    assert_eq!(get(at, "/api/v1/panel_status").await.json()["connected"], true);
    studio.stop().await;
}

/// Containment. A piece that panics and one that stalls are each caught,
/// logged once, and replaced by the fallback; the server and the other players
/// carry on. Neither piece ships in the normal list.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bad_piece_is_contained_and_the_rest_carries_on() {
    let dir = Temp::new("faults");
    let (_bad_sim, bad_port) = start_sim("bad001");
    let (_good_sim, good_port) = start_sim("good01");
    let studio = studio_in(&dir.0, true).await;
    let at = studio.addr;

    post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{bad_port}","name":"bad","play":true}}"#)).await;
    post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{good_port}","name":"good","play":true}}"#)).await;
    until(Duration::from_secs(20), "both panels to resolve", || async {
        let s = get(at, "/api/v1/status").await.json();
        s["devices"].as_array().is_some_and(|d| d.len() == 2 && d.iter().all(|x| x["resolved"] == true))
    })
    .await;
    post(at, "/api/v1/player/set", r#"{"device":"good01","piece":"plasma"}"#).await;

    for (bad, kind) in [("fault-panic", "panicking"), ("fault-stall", "stalling")] {
        let set = post(at, "/api/v1/player/set", &format!(r#"{{"device":"bad001","piece":"{bad}","seed":1}}"#)).await;
        assert_eq!(set.status, 200, "the fault pieces should be offered here: {}", String::from_utf8_lossy(&set.body));

        // The watchdog is 5 s, so a stall takes a little longer to notice - and
        // the answer that says it has been replaced is the one every assertion
        // below is about, rather than a second read taken afterwards.
        let s = until_json(
            at,
            Duration::from_secs(30),
            &format!("the {kind} piece to be replaced"),
            "/api/v1/status",
            |s| {
                let bad_dev = s["devices"].as_array().and_then(|d| d.iter().find(|x| x["id"] == "bad001")).cloned().unwrap_or_default();
                bad_dev["player"]["piece"] != bad && bad_dev["player"]["running"] == true
            },
        )
        .await;

        let bad_dev = s["devices"].as_array().and_then(|d| d.iter().find(|x| x["id"] == "bad001")).cloned().expect("the bad panel");
        let good_dev = s["devices"].as_array().and_then(|d| d.iter().find(|x| x["id"] == "good01")).cloned().expect("the good panel");
        assert_ne!(bad_dev["player"]["piece"], bad, "the bad piece is still loaded: {bad_dev}");
        assert_eq!(bad_dev["player"]["health"]["fell_back_from"], bad, "{bad_dev}");
        assert_eq!(bad_dev["player"]["running"], true, "the bad panel's player did not come back: {bad_dev}");

        // The other player never noticed.
        assert_eq!(good_dev["player"]["piece"], "plasma", "the other player was disturbed: {good_dev}");
        assert_eq!(good_dev["player"]["running"], true, "the other player stopped: {good_dev}");
        assert_eq!(good_dev["player"]["health"]["panics"], 0);
        assert_eq!(good_dev["player"]["health"]["stalls"], 0);

        // And the server itself is still well: a contained fault is not a 503.
        let h = get(at, "/healthz").await;
        assert_eq!(h.status, 200, "a contained fault should not make the server unhealthy: {}", String::from_utf8_lossy(&h.body));

        println!(
            "{kind}: bad panel now plays {} (panics {}, stalls {}, restarts {}, abandoned {}); good panel still on {} with {} frames sent",
            bad_dev["player"]["piece"],
            bad_dev["player"]["health"]["panics"],
            bad_dev["player"]["health"]["stalls"],
            bad_dev["player"]["health"]["restarts"],
            bad_dev["player"]["health"]["abandoned"],
            good_dev["player"]["piece"],
            good_dev["player"]["panel"]["frames_sent"],
        );
    }
    studio.stop().await;
}

/// A studio that was never told about the fault pieces will not load one, and
/// says so rather than dying.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fault_pieces_are_not_on_the_menu() {
    let dir = Temp::new("nofaults");
    let (_sim, port) = start_sim("aa55aa");
    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    add_and_play(at, port).await;
    until(Duration::from_secs(20), "the panel to resolve", || async { device_status(at).await["resolved"] == true }).await;

    let refused = post(at, "/api/v1/player/set", r#"{"device":"aa55aa","piece":"fault-panic"}"#).await;
    assert_eq!(refused.status, 400, "{}", String::from_utf8_lossy(&refused.body));
    assert!(String::from_utf8_lossy(&refused.body).contains("fault-panic"));

    // And it is not in the piece list a browser is handed either.
    let pieces = get(at, "/api/v1/bootstrap").await.json()["pieces"].clone();
    let ids: Vec<String> = pieces.as_array().expect("a list").iter().map(|p| p["id"].as_str().unwrap_or("").to_string()).collect();
    assert!(!ids.iter().any(|i| i.starts_with("fault-")), "{ids:?}");
    studio.stop().await;
}

/// `/healthz` is about the server, not about the panels. A studio with no
/// panel at all, and one whose panel has gone away mid-stream, are both 200.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_missing_panel_is_never_unhealthy() {
    let dir = Temp::new("healthz");
    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;

    // Nothing configured at all.
    let h = get(at, "/healthz").await;
    assert_eq!(h.status, 200);
    assert_eq!(String::from_utf8_lossy(&h.body).trim(), "ok");

    // A panel that has never existed.
    post(at, "/api/v1/devices/add", r#"{"to":"127.0.0.1:1","name":"ghost","play":true}"#).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let h = get(at, "/healthz").await;
    assert_eq!(h.status, 200, "a panel that is not there must not make the server unhealthy: {}", String::from_utf8_lossy(&h.body));
    let s = get(at, "/api/v1/status").await.json();
    assert_eq!(s["ok"], true, "{s}");
    assert_eq!(s["problems"].as_array().map(Vec::len), Some(0));

    // A panel that went away mid-stream.
    let (sim, port) = start_sim("aa66aa");
    post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{port}","name":"real","play":true}}"#)).await;
    until(Duration::from_secs(20), "the real panel to stream", || async {
        let s = get(at, "/api/v1/status").await.json();
        s["devices"].as_array().is_some_and(|d| d.iter().any(|x| x["player"]["panel"]["connected"] == true))
    })
    .await;
    drop(sim);
    tokio::time::sleep(Duration::from_secs(8)).await;
    let h = get(at, "/healthz").await;
    assert_eq!(h.status, 200, "a panel that vanished must not make the server unhealthy: {}", String::from_utf8_lossy(&h.body));
    println!("healthz with a dead panel: {} {}", h.status, String::from_utf8_lossy(&h.body).trim());
    studio.stop().await;
}

/// **Card 143's acceptance, which card 170 closes by construction**: the piece
/// the page is showing is a player's, so a piece that stops returning is
/// caught by the same watchdog every panel has, replaced by the fallback, and
/// the page is drawing again within seconds - without a 503 and without a
/// restart.
///
/// Card 106's version of this test asserted the opposite, because the design
/// view's engine had no stall recovery: it waited for `/healthz` to go 503 and
/// stay there. What survives is the part that mattered at three in the
/// morning - **`/api/v1/status` answers while a piece is stuck** - and it is
/// now true for a better reason. A player's core is owned by its render thread
/// and is behind no shared lock at all, so there is nothing for a wedged piece
/// to hold.
///
/// The latency bound is measured against a baseline taken in this same test,
/// so a loaded host makes both numbers bigger and the assertion still means
/// "the wedge cost nothing".
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_wedged_piece_is_replaced_and_the_status_route_never_stalls() {
    let dir = Temp::new("wedged");
    let studio = studio_in(&dir.0, true).await;
    let at = studio.addr;

    // How long this machine takes to answer at all, right now.
    let baseline = {
        let started = std::time::Instant::now();
        for _ in 0..5 {
            assert_eq!(get(at, "/api/v1/status").await.status, 200);
        }
        started.elapsed() / 5
    };

    let set = post(at, "/api/v1/set_piece", r#"{"id":"fault-stall"}"#).await;
    assert_eq!(set.status, 200, "{}", String::from_utf8_lossy(&set.body));

    // `fault-stall` stops returning on its fourth frame and stays stopped for
    // 30 s. Through the whole of the window that contains the stall and the
    // watchdog's answer to it, keep asking - and time every answer.
    let mut worst = Duration::ZERO;
    let mut asked = 0u32;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let started = std::time::Instant::now();
        let r = get(at, "/api/v1/status").await;
        worst = worst.max(started.elapsed());
        asked += 1;
        assert_eq!(r.status, 200, "the status route stopped answering while a piece was stuck");
        assert_eq!(
            get(at, "/healthz").await.status,
            200,
            "a piece that stalls is recovered, not a reason to restart the container"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let allowed = (baseline * 25).max(Duration::from_secs(1));
    assert!(
        worst < allowed,
        "the status route took {worst:?} at worst while a piece was stuck (baseline {baseline:?}, allowed {allowed:?})"
    );

    // And it recovered by itself, on the fallback piece, with nobody asked.
    let s = until_json(at, Duration::from_secs(30), "the wedged piece to be replaced", "/api/v1/status", |s| {
        s["preview"]["piece"] != "fault-stall" && s["preview"]["alive"] == true && s["preview"]["wedged"] == false
    })
    .await;
    assert_eq!(s["preview"]["fell_back_from"], "fault-stall", "{}", s["preview"]);
    assert_eq!(s["ok"], true, "a recovered stall is not an unwell server: {}", s["problems"]);
    println!(
        "wedged piece: {asked} status reads while it was stuck, worst {worst:?} (baseline {baseline:?}); \
         recovered onto `{}` with {} stalls counted",
        s["preview"]["piece"], s["devices"]
    );

    // Dropped rather than stopped on purpose: the abandoned thread is still
    // inside its 30 s sleep and no thread can be killed in Rust. Nothing is
    // waiting on it - that is the point of abandoning it - and the process
    // ends long before it matters.
    drop(studio);
}

/// A state file that cannot be written is the other 503: a studio that cannot
/// save will not come back as itself, which is the whole promise of the card.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_state_directory_that_cannot_be_used_is_a_503() {
    let dir = Temp::new("unwritable");
    // A *file* where the state directory should be: `create_dir_all` cannot
    // win, and neither can any write after it.
    let blocked = dir.0.join("in-the-way");
    std::fs::write(&blocked, b"not a directory").expect("write");
    let studio = studio_in(&blocked, false).await;
    let at = studio.addr;

    until(Duration::from_secs(10), "healthz to notice the unwritable state", || async {
        get(at, "/healthz").await.status == 503
    })
    .await;
    let h = get(at, "/healthz").await;
    let body = String::from_utf8_lossy(&h.body).to_string();
    assert!(body.contains("state file"), "{body}");
    let s = get(at, "/api/v1/status").await.json();
    assert_eq!(s["ok"], false);
    assert!(s["state"]["last_error"].is_string(), "{}", s["state"]);
    println!("503 body: {}", body.replace('\n', " | "));

    // But the server is still a server: it serves, and it still plays.
    assert_eq!(get(at, "/api/v1/bootstrap").await.status, 200);
    assert_eq!(get(at, "/").await.status, 200);
    studio.stop().await;
}

/// The device controls, all five, through the API and into the simulator - and
/// the one rule that matters for a panel running off a laptop's USB port:
/// brightness is never raised above the cap the device reports.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn the_device_controls_reach_the_device() {
    let dir = Temp::new("controls");
    let (sim, port) = start_sim("aa88aa");
    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    add_and_play(at, port).await;
    until(Duration::from_secs(20), "the panel to resolve", || async { device_status(at).await["resolved"] == true }).await;

    // Brightness: the simulator's cap is 120, so asking for 200 applies 120.
    let b = post(at, "/api/v1/device/brightness", r#"{"device":"aa88aa","level":200}"#).await;
    assert_eq!(b.status, 200, "{}", String::from_utf8_lossy(&b.body));
    assert_eq!(b.json()["asked"], 200);
    assert_eq!(b.json()["applied"], 120, "the device's cap should come back: {}", b.json());
    let b = post(at, "/api/v1/device/brightness", r#"{"device":"aa88aa","level":40}"#).await;
    assert_eq!(b.json()["applied"], 40);
    until(PATIENCE, "the device to be at 40", || async { sim.handle().telemetry().brightness == 40 }).await;

    // Identify.
    let i = post(at, "/api/v1/device/identify", r#"{"device":"aa88aa","ms":500}"#).await;
    assert_eq!(i.status, 200, "{}", String::from_utf8_lossy(&i.body));

    // Name: on the device, and here.
    let n = post(at, "/api/v1/device/name", r#"{"device":"aa88aa","name":"the desk one"}"#).await;
    assert_eq!(n.status, 200, "{}", String::from_utf8_lossy(&n.body));
    assert_eq!(n.json()["on_the_device"], true);
    assert_eq!(device_status(at).await["label"], "the desk one");

    // Stats: a live telemetry read.
    let s = post(at, "/api/v1/device/stats", r#"{"device":"aa88aa"}"#).await;
    assert_eq!(s.status, 200, "{}", String::from_utf8_lossy(&s.body));
    assert_eq!(s.json()["brightness"], 40);
    assert!(s.json()["uptime_s"].is_number());
    assert_eq!(s.json()["rssi_dbm"], -55);

    // Reboot needs confirming.
    let r = post(at, "/api/v1/device/reboot", r#"{"device":"aa88aa"}"#).await;
    assert_eq!(r.status, 400, "reboot without confirmation should be refused: {}", String::from_utf8_lossy(&r.body));
    let r = post(at, "/api/v1/device/reboot", r#"{"device":"aa88aa","confirm":true}"#).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));

    // A device that is not there is a 404, and one that cannot be reached is a
    // 409 - neither is a 500, and neither is a reason for the server to be ill.
    assert_eq!(post(at, "/api/v1/device/identify", r#"{"device":"nope"}"#).await.status, 404);
    post(at, "/api/v1/devices/add", r#"{"to":"127.0.0.1:2","name":"ghost"}"#).await;
    assert_eq!(post(at, "/api/v1/device/identify", r#"{"device":"pending:127.0.0.1:2"}"#).await.status, 409);
    assert_eq!(get(at, "/healthz").await.status, 200);
    studio.stop().await;
}

/// The brightness policy is re-applied when the link comes back, because a
/// panel that reboots comes back at its own default.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn the_brightness_policy_survives_the_panel_rebooting() {
    let dir = Temp::new("brightness");
    let (sim, port) = start_sim("aa99aa");
    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    add_and_play(at, port).await;
    wait_until_streaming(at, "the panel to stream").await;

    // Nothing has a brightness policy yet, so this reading is the panel's own
    // and nothing is racing to change it.
    let before = sim.handle().telemetry().brightness;
    assert_ne!(before, 33, "the test needs a level the panel is not already on");

    post(at, "/api/v1/device/brightness", r#"{"device":"aa99aa","level":33}"#).await;
    until(PATIENCE, "the panel to take the brightness", || async { sim.handle().telemetry().brightness == 33 }).await;

    // The panel goes away and comes back. A fresh simulator has no memory of
    // anything - that is the whole reason the policy has to be re-applied -
    // so it comes up on its own default again. Its brightness is deliberately
    // *not* read here: the studio may have put it right already, and racing
    // that would be the flake rather than the test.
    drop(sim);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let sim = sim_on(port, "aa99aa").expect("the panel comes back");

    until(Duration::from_secs(30), "the brightness policy to be re-applied", || async {
        sim.handle().telemetry().brightness == 33
    })
    .await;
    println!("brightness policy re-applied after a panel reboot: {before} -> 33 -> reboot -> {}", sim.handle().telemetry().brightness);
    studio.stop().await;
}

/// A state file that cannot be used must not stop the server, end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broken_state_file_starts_a_working_server() {
    for (tag, content) in [("corrupt-e2e", "{{{{"), ("truncated-e2e", r#"{"version":1,"players":[{"dev"#), ("future-e2e", r#"{"version":99}"#)] {
        let dir = Temp::new(tag);
        std::fs::write(dir.0.join("state.json"), content).expect("write");
        let studio = studio_in(&dir.0, false).await;
        let at = studio.addr;
        assert_eq!(get(at, "/healthz").await.status, 200, "{tag}");
        let s = get(at, "/api/v1/status").await.json();
        assert_eq!(s["ok"], true, "{tag}: {s}");
        assert!(s["state"]["recovered"].is_string(), "{tag}: it should say why: {}", s["state"]);
        assert_eq!(s["devices"].as_array().map(Vec::len), Some(0), "{tag}");
        // And it can write again from here.
        post(at, "/api/v1/set_seed", r#"{"seed":5}"#).await;
        until(PATIENCE, "the state file to be written again", || async {
            get(at, "/api/v1/status").await.json()["state"]["writes"].as_u64().unwrap_or(0) > 0
        })
        .await;
        studio.stop().await;
    }
}
