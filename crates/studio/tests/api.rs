//! The studio as a browser sees it: the API round-trips, the UI is served,
//! and the socket pushes frames and other people's changes.
//!
//! Every server here is bound to an ephemeral loopback port and stopped when
//! the test ends. **Nothing in this file touches the network.**

mod common;

use common::{get, post, post_as, preview_of, seq_of, studio, until, Msg, Ws, PATIENCE};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_api_round_trips() {
    let studio = studio().await;
    let at = studio.addr;

    // bootstrap: every patch, the payload budget, the current state.
    let boot = get(at, "/api/v1/bootstrap").await;
    assert_eq!(boot.status, 200);
    let boot = boot.json();
    assert!(boot["patches"].as_array().expect("a list of patches").len() >= 4);
    assert_eq!(boot["payload_bytes"], 1464, "the spec's pixel payload");
    let first = boot["state"]["patch"].as_str().expect("a patch").to_string();

    // set_patch, and the error a typo gets.
    let chosen = post(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await;
    assert_eq!(chosen.status, 200);
    assert_eq!(chosen.json()["patch"], "metaballs");
    let wrong = post(at, "/api/v1/set_patch", r#"{"id":"no-such-patch"}"#).await;
    assert_eq!(wrong.status, 400);
    assert_eq!(wrong.json()["error"], "no patch called `no-such-patch`");
    assert_ne!(first, "metaballs", "this test assumes the studio does not start on metaballs");

    // set_param, clamped by the patch's own spec, and the error for a typo.
    let set = post(at, "/api/v1/set_param", r#"{"id":"size","value":2.0}"#).await;
    assert_eq!(set.status, 200);
    assert_eq!(set.json()["params"]["size"], 2.0);
    let wrong = post(at, "/api/v1/set_param", r#"{"id":"nonesuch","value":1.0}"#).await;
    assert_eq!(wrong.status, 400);
    assert_eq!(wrong.json()["error"], "metaballs has no parameter `nonesuch`");

    // reset_params puts it back where the patch asked.
    let reset = post(at, "/api/v1/reset_params", "{}").await;
    assert_eq!(reset.status, 200);
    let default_size = reset.json()["params"]["size"].clone();
    assert_ne!(default_size, 2.0);

    // set_seed: given one, or told to find one.
    let seeded = post(at, "/api/v1/set_seed", r#"{"seed":4242}"#).await.json();
    assert_eq!(seeded["seed"], 4242);
    let fresh = post(at, "/api/v1/set_seed", r#"{"seed":null}"#).await.json();
    assert_ne!(fresh["seed"], 4242, "a null seed means a new one");

    // set_output: the panel model the preview is drawn through.
    let mut output = fresh["output"].clone();
    output["panel"] = "bit_planes".into();
    output["dither"] = "bayer4".into();
    output["limiter"]["enabled"] = false.into();
    let body = serde_json::json!({ "output": output }).to_string();
    let after = post(at, "/api/v1/set_output", &body).await;
    assert_eq!(after.status, 200);
    let after = after.json();
    assert_eq!(after["output"]["panel"], "bit_planes");
    assert_eq!(after["output"]["dither"], "bayer4");
    assert_eq!(after["output"]["limiter"]["enabled"], false);

    // set_playback, with the speed clamped to the player's range. Card 161:
    // `fps` is not a field of this body any more, and one that still carries
    // it is accepted with the field ignored - `tests/ui.rs` has that test.
    let play = post(at, "/api/v1/set_playback", r#"{"paused":true,"speed":99.0}"#).await.json();
    assert_eq!(play["paused"], true);
    assert_eq!(play["speed"], 8.0, "speed is clamped to 8x");
    assert_eq!(play["fps"], 30.0, "and the one rate is reported back");
    let play = post(at, "/api/v1/set_playback", r#"{"paused":false,"speed":1.0}"#).await.json();
    assert_eq!(play["fps"], 30.0, "which nothing can move");

    // restart keeps the seed and starts the patch's clock again.
    let restarted = post(at, "/api/v1/restart", "{}").await;
    assert_eq!(restarted.status, 200);
    assert_eq!(restarted.json()["seed"], fresh["seed"]);

    // patch_playing / patch_act: metaballs composes nothing, so both are null.
    let playing = get(at, "/api/v1/patch_playing").await;
    assert_eq!(playing.status, 200);
    assert_eq!(playing.json(), serde_json::Value::Null);
    let acted = post(at, "/api/v1/patch_act", r#"{"action":"next"}"#).await;
    assert_eq!(acted.status, 200);

    // A patch that does compose answers both - once it has rendered a frame
    // and so knows what it is doing.
    post(at, "/api/v1/set_patch", r#"{"id":"clocks-numerals"}"#).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let playing = get(at, "/api/v1/patch_playing").await.json();
    assert!(playing["title"].is_string(), "clocks-numerals says what it is performing: {playing}");

    // panel_status: off until something turns it on.
    assert_eq!(get(at, "/api/v1/panel_status").await.json(), serde_json::Value::Null);

    // frame: the packet the preview is drawn from, and nothing else.
    let frame = get(at, "/api/v1/frame").await;
    assert_eq!(frame.status, 200);
    assert_eq!(frame.body.len(), screeny_studio::page::PACKET_BYTES);
}

/// The studio renders whether or not a browser is watching; the frame it
/// serves keeps moving. (Slowly: with no panel connected and no socket open a
/// player drops to `player::IDLE_FPS`, which is the whole point - a panel that
/// is away for a month with nobody looking must not cost a core for a month.
/// A second is several frames at that rate.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_player_renders_with_nobody_watching() {
    let studio = studio().await;
    let first = seq_of(&get(studio.addr, "/api/v1/frame").await.body);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let later = seq_of(&get(studio.addr, "/api/v1/frame").await.body);
    assert!(later > first, "the render loop stopped: {first} -> {later}");
}

/// ...and it speeds up the moment somebody is. With nobody there a player
/// idles at `player::IDLE_FPS`; with somebody watching it runs at the full
/// rate, which since card 161 is `screeny_art::FPS` and nothing else.
///
/// The socket asks for everything (`fps=60` is clamped to the render rate,
/// which is all there is to send). That is the browser's side of the bargain
/// and not the player's: what is being measured here is that the *player* is
/// rendering fast.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watching_browser_gets_the_full_rate() {
    let studio = studio().await;
    let at = studio.addr;
    let mut ws = Ws::connect_asking(at, "fps=60").await;
    ws.frame().await;
    let n = ws.count_frames(Duration::from_secs(1)).await;
    // Generously clear of IDLE_FPS (5) and below the 30 it is aiming at: this
    // runs on a loaded bench.
    assert!(n > 18, "only {n} frames in a second with a browser watching");
}

/// A studio that has never seen a panel still has a picture, and says plainly
/// that there is no panel rather than pretending there is one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_studio_with_no_panel_still_plays_something() {
    let studio = studio().await;
    let at = studio.addr;

    let boot = get(at, "/api/v1/bootstrap").await.json();
    assert!(boot["state"]["patch"].as_str().is_some_and(|p| !p.is_empty()));
    assert_eq!(boot["state"]["device"], "", "nothing is attached");
    assert_eq!(get(at, "/api/v1/panel_status").await.json(), serde_json::Value::Null);

    let s = get(at, "/api/v1/status").await.json();
    assert_eq!(s["ok"], true, "no panel is not a fault: {s}");
    assert_eq!(s["preview"]["device"], "");
    assert_eq!(s["preview"]["panel_to"], "");
    assert_eq!(s["devices"].as_array().map(Vec::len), Some(0));
    // And it is really rendering, not idling on a black frame.
    until(PATIENCE, "the unattached studio to render", || async {
        get(at, "/api/v1/status").await.json()["preview"]["alive"] == true
    })
    .await;
}

/// The UI is in the binary, one directory deep, and nothing else is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ui_is_served_from_the_binary() {
    let studio = studio().await;
    let at = studio.addr;

    let index = get(at, "/").await;
    assert_eq!(index.status, 200);
    assert!(String::from_utf8_lossy(&index.body).contains("Screeny Studio"));
    // Card 198: two screens, three scripts.
    assert_eq!(get(at, "/picture.js").await.status, 200);
    assert_eq!(get(at, "/common.js").await.status, 200);
    assert_eq!(get(at, "/panel").await.status, 200);
    assert_eq!(get(at, "/panel.js").await.status, 200);
    assert_eq!(get(at, "/style.css").await.status, 200);

    assert_eq!(get(at, "/nope.js").await.status, 404);
    assert_eq!(get(at, "/../Cargo.toml").await.status, 404, "no traversal out of the UI");
    assert_eq!(get(at, "/sub/dir.js").await.status, 404, "the UI is one directory, flat");
}

/// A container healthcheck has something cheap to ask.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthz_answers() {
    let studio = studio().await;
    let health = get(studio.addr, "/healthz").await;
    assert_eq!(health.status, 200);
    assert_eq!(health.body, b"ok\n");
}

/// The socket delivers frames, and they are the frames the engine is making.
/// It asks for everything for the same reason as above: this is about what the
/// engine makes, not about what a browser chooses to be sent (card 120).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_socket_delivers_frames() {
    let studio = studio().await;
    let mut ws = Ws::connect_asking(studio.addr, "client=watcher&fps=60").await;

    // The first thing a browser gets is the state, so it need not ask.
    let hello = ws.event("state").await;
    assert!(hello["state"]["patch"].is_string());

    let a = ws.frame().await;
    assert_eq!(a.len(), screeny_studio::page::PACKET_BYTES);
    let b = ws.frame().await;
    assert!(seq_of(&b) > seq_of(&a), "frames did not advance: {} -> {}", seq_of(&a), seq_of(&b));

    // At the full rate a second should bring a lot more than a handful.
    let n = ws.count_frames(Duration::from_secs(1)).await;
    assert!(n > 18, "only {n} frames in a second");
}

/// Two browsers, and what one does the other is told about - without the one
/// that did it being told about its own change, which would fight with the
/// control still under the mouse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_browsers_see_each_others_changes() {
    let studio = studio().await;
    let at = studio.addr;
    let mut alice = Ws::connect(at, Some("alice")).await;
    let mut bob = Ws::connect(at, Some("bob")).await;
    alice.event("state").await; // the hello
    bob.event("state").await;

    // Alice changes the patch and the seed.
    post_as(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#, Some("alice")).await;
    post_as(at, "/api/v1/set_seed", r#"{"seed":1234}"#, Some("alice")).await;

    // Bob is told, in order, and the event says who did it.
    let ev = bob.event("state").await;
    assert_eq!(ev["state"]["patch"], "metaballs");
    assert_eq!(ev["from"], "alice");
    let ev = bob.event("state").await;
    assert_eq!(ev["state"]["seed"], 1234);
    assert!(ev["rev"].as_u64().expect("a revision") >= 2);

    // Alice is not told about Alice. Give the server a moment to be wrong.
    let quiet = tokio::time::timeout(Duration::from_millis(600), async {
        loop {
            match alice.next().await {
                Some(Msg::Text(t)) => {
                    let v: serde_json::Value = serde_json::from_str(&t).expect("valid JSON");
                    if v["type"] == "state" {
                        return v;
                    }
                }
                Some(_) => {}
                None => return serde_json::Value::Null,
            }
        }
    })
    .await;
    assert!(quiet.is_err(), "the studio echoed Alice her own change: {quiet:?}");

    // And Bob changing something reaches Alice, so it is not one-way.
    post_as(at, "/api/v1/set_playback", r#"{"paused":true,"speed":1.0}"#, Some("bob")).await;
    let ev = alice.event("state").await;
    assert_eq!(ev["state"]["paused"], true);
    assert_eq!(ev["from"], "bob");
}

/// A browser that arrives late is handed the state as it is now, and one that
/// does not name itself is told about everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_browser_starts_in_step() {
    let studio = studio().await;
    let at = studio.addr;
    post(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await;
    post(at, "/api/v1/set_seed", r#"{"seed":77}"#).await;

    let mut late = Ws::connect(at, None).await;
    let hello = tokio::time::timeout(PATIENCE, late.event("state")).await.expect("a hello");
    assert_eq!(hello["state"]["patch"], "metaballs");
    assert_eq!(hello["state"]["seed"], 77);
    assert_eq!(hello["from"], serde_json::Value::Null, "a resync is for everyone");
}

/// The heartbeat: twice a second, what the patch is performing and what the
/// panel link is doing. It is what replaced the UI's two polling timers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_socket_carries_the_heartbeat() {
    let studio = studio().await;
    post(studio.addr, "/api/v1/set_patch", r#"{"id":"clocks-numerals"}"#).await;
    let mut ws = Ws::connect(studio.addr, None).await;
    // Heartbeats are taken while the engine is between frames, so the first
    // one after a patch is rebuilt can legitimately have nothing to perform
    // yet. What is being pinned is that the heartbeat carries it at all.
    let mut status = ws.event("status").await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !status["playing"]["title"].is_string() && tokio::time::Instant::now() < deadline {
        status = ws.event("status").await;
    }
    assert!(status["playing"]["title"].is_string(), "the heartbeat carries now playing: {status}");
    assert_eq!(status["panel"], serde_json::Value::Null, "nothing is being sent");
}

/// Stopping the studio is not held up by a browser that is perfectly happy.
/// It matters because stopping is what releases the panel: `docker stop` sends
/// `SIGTERM`, and a shutdown that waited for every open preview socket to go
/// away by itself would wait for ever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connected_browser_does_not_hold_the_shutdown_open() {
    let studio = studio().await;
    let mut ws = Ws::connect(studio.addr, None).await;
    ws.frame().await; // it is really connected and really being fed

    drop(studio); // as a signal would

    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Msg::Close) | None => return true,
                Some(_) => {}
            }
        }
    })
    .await;
    assert_eq!(ended, Ok(true), "the socket was still open after the studio stopped");
}

/// The preview a browser is shown is the frame the engine measured: header
/// first, then exactly one 64x32 picture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frame_packet_is_a_header_and_a_picture() {
    let studio = studio().await;
    let mut ws = Ws::connect(studio.addr, None).await;
    let packet = ws.frame().await;
    assert_eq!(preview_of(&packet).len(), 64 * 32 * 3);
    let colours = u32::from_le_bytes(packet[8..12].try_into().unwrap());
    let bytes = u32::from_le_bytes(packet[12..16].try_into().unwrap());
    assert!(colours > 0 && colours <= 256, "distinct colours: {colours}");
    assert!(bytes > 0 && bytes <= 1464, "encoded bytes: {bytes}");
}

// ------------------ card 150: the names a piece went by ------------------

/// **Every old route still answers**, on the same handler as its new name.
///
/// The card's promise is that nothing that works today stops working: shell
/// scripts drive this API directly, and a script written
/// before card 150 says `set_piece`, `set_settings`, `piece_playing` and
/// `piece_act`. What comes *back* is the new vocabulary, which is the other
/// half of the promise - one name per thing, on the way out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_routes_a_piece_had_still_answer() {
    let studio = studio().await;
    let at = studio.addr;

    // POST /set_piece, with `id` as it always took it.
    let r = post(at, "/api/v1/set_piece", r#"{"id":"clocks-dials"}"#).await;
    assert_eq!(r.status, 200);
    let state = r.json();
    assert_eq!(state["patch"], "clocks-dials");
    assert!(state.get("piece").is_none(), "the reply says `patch` and only `patch`: {state}");
    assert!(state.get("output").is_some(), "and `output`: {state}");
    // Card 150 freed the word `settings` for card 151, which took it: in a
    // reply it is now a patch's **named settings**, a list of names, and never
    // the output block under its old name.
    assert!(state["settings"].is_array(), "`settings` is the patch's named settings now: {state}");

    // The card's own acceptance line: the body may name the patch `piece`.
    let r = post(at, "/api/v1/set_piece", r#"{"piece":"metaballs"}"#).await;
    assert_eq!(r.status, 200, "{:?}", r.json());
    assert_eq!(r.json()["patch"], "metaballs");
    // ...or `patch`, on either route.
    assert_eq!(post(at, "/api/v1/set_patch", r#"{"patch":"clocks-dials"}"#).await.json()["patch"], "clocks-dials");

    // POST /set_settings, with the block still called `settings`.
    let mut output = state["output"].clone();
    output["dither"] = "bayer8".into();
    let body = serde_json::json!({ "settings": output }).to_string();
    let after = post(at, "/api/v1/set_settings", &body).await;
    assert_eq!(after.status, 200);
    assert_eq!(after.json()["output"]["dither"], "bayer8");

    // GET /piece_playing and POST /piece_act.
    assert_eq!(get(at, "/api/v1/piece_playing").await.status, 200);
    assert_eq!(post(at, "/api/v1/piece_act", r#"{"action":"next"}"#).await.status, 200);
}

/// `POST /player/set` is what another session's scripts drive, so both of its
/// renamed fields are taken under either name - and its answer, like every
/// other, carries the new ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn player_set_takes_piece_and_settings_too() {
    let studio = studio().await;
    let at = studio.addr;
    let id = post(at, "/api/v1/devices/add", r#"{"to":"127.0.0.1:49999","name":"ghost"}"#).await.json()["id"]
        .as_str()
        .expect("a device id")
        .to_string();

    let old = format!(r#"{{"device":"{id}","piece":"clocks-dials","seed":7,"settings":{{"dither":"bayer8"}}}}"#);
    let r = post(at, "/api/v1/player/set", &old).await;
    assert_eq!(r.status, 200, "{:?}", r.json());
    let status = r.json();
    assert_eq!(status["patch"], "clocks-dials");
    assert_eq!(status["seed"], 7);
    assert_eq!(status["output"]["dither"], "bayer8");
    assert!(status.get("piece").is_none() && status.get("settings").is_none(), "the reply is in one vocabulary: {status}");

    let new = format!(r#"{{"device":"{id}","patch":"metaballs","output":{{"dither":"bayer4"}}}}"#);
    let status = post(at, "/api/v1/player/set", &new).await.json();
    assert_eq!(status["patch"], "metaballs");
    assert_eq!(status["output"]["dither"], "bayer4");
}
