//! The studio as a browser sees it: the API round-trips, the UI is served,
//! and the socket pushes frames and other people's changes.
//!
//! Every server here is bound to an ephemeral loopback port and stopped when
//! the test ends. **Nothing in this file touches the network.**

mod common;

use common::{get, post, post_as, preview_of, seq_of, studio, Msg, Ws, PATIENCE};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_api_round_trips() {
    let studio = studio().await;
    let at = studio.addr;

    // bootstrap: every piece, the payload budget, the current state.
    let boot = get(at, "/api/v1/bootstrap").await;
    assert_eq!(boot.status, 200);
    let boot = boot.json();
    assert!(boot["pieces"].as_array().expect("a list of pieces").len() >= 4);
    assert_eq!(boot["payload_bytes"], 1464, "the spec's pixel payload");
    let first = boot["state"]["piece"].as_str().expect("a piece").to_string();

    // set_piece, and the error a typo gets.
    let plasma = post(at, "/api/v1/set_piece", r#"{"id":"plasma"}"#).await;
    assert_eq!(plasma.status, 200);
    assert_eq!(plasma.json()["piece"], "plasma");
    let wrong = post(at, "/api/v1/set_piece", r#"{"id":"no-such-piece"}"#).await;
    assert_eq!(wrong.status, 400);
    assert_eq!(wrong.json()["error"], "no piece called `no-such-piece`");
    assert_ne!(first, "plasma", "this test assumes the studio does not start on plasma");

    // set_param, clamped by the piece's own spec, and the error for a typo.
    let set = post(at, "/api/v1/set_param", r#"{"id":"scale","value":2.0}"#).await;
    assert_eq!(set.status, 200);
    assert_eq!(set.json()["params"]["scale"], 2.0);
    let wrong = post(at, "/api/v1/set_param", r#"{"id":"nonesuch","value":1.0}"#).await;
    assert_eq!(wrong.status, 400);
    assert_eq!(wrong.json()["error"], "plasma has no parameter `nonesuch`");

    // reset_params puts it back where the piece asked.
    let reset = post(at, "/api/v1/reset_params", "{}").await;
    assert_eq!(reset.status, 200);
    let default_scale = reset.json()["params"]["scale"].clone();
    assert_ne!(default_scale, 2.0);

    // set_seed: given one, or told to find one.
    let seeded = post(at, "/api/v1/set_seed", r#"{"seed":4242}"#).await.json();
    assert_eq!(seeded["seed"], 4242);
    let fresh = post(at, "/api/v1/set_seed", r#"{"seed":null}"#).await.json();
    assert_ne!(fresh["seed"], 4242, "a null seed means a new one");

    // set_settings: the panel model the preview is drawn through.
    let mut settings = fresh["settings"].clone();
    settings["levels"] = 32.into();
    settings["dither"] = "bayer4".into();
    settings["limiter"]["enabled"] = false.into();
    let body = serde_json::json!({ "settings": settings }).to_string();
    let after = post(at, "/api/v1/set_settings", &body).await;
    assert_eq!(after.status, 200);
    let after = after.json();
    assert_eq!(after["settings"]["levels"], 32);
    assert_eq!(after["settings"]["dither"], "bayer4");
    assert_eq!(after["settings"]["limiter"]["enabled"], false);

    // set_playback, with the speed clamped and an unknown rate ignored.
    let play = post(at, "/api/v1/set_playback", r#"{"paused":true,"speed":99.0,"fps":30.0}"#).await.json();
    assert_eq!(play["paused"], true);
    assert_eq!(play["speed"], 8.0, "speed is clamped to 8x");
    assert_eq!(play["fps"], 30.0);
    let play = post(at, "/api/v1/set_playback", r#"{"paused":false,"speed":1.0,"fps":45.0}"#).await.json();
    assert_eq!(play["fps"], 30.0, "45 is not one of the offered rates, so the rate does not move");

    // restart keeps the seed and starts the piece's clock again.
    let restarted = post(at, "/api/v1/restart", "{}").await;
    assert_eq!(restarted.status, 200);
    assert_eq!(restarted.json()["seed"], fresh["seed"]);

    // piece_playing / piece_act: plasma composes nothing, so both are null.
    let playing = get(at, "/api/v1/piece_playing").await;
    assert_eq!(playing.status, 200);
    assert_eq!(playing.json(), serde_json::Value::Null);
    let acted = post(at, "/api/v1/piece_act", r#"{"action":"next"}"#).await;
    assert_eq!(acted.status, 200);

    // A piece that does compose answers both - once it has rendered a frame
    // and so knows what it is doing.
    post(at, "/api/v1/set_piece", r#"{"id":"clocks-numerals"}"#).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let playing = get(at, "/api/v1/piece_playing").await.json();
    assert!(playing["title"].is_string(), "clocks-numerals says what it is performing: {playing}");

    // panel_status: off until something turns it on.
    assert_eq!(get(at, "/api/v1/panel_status").await.json(), serde_json::Value::Null);

    // frame: the packet the preview is drawn from, and nothing else.
    let frame = get(at, "/api/v1/frame").await;
    assert_eq!(frame.status, 200);
    assert_eq!(frame.body.len(), screeny_studio::engine::PACKET_BYTES);
}

/// The engine runs whether or not a browser is watching; the frame it serves
/// keeps moving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_engine_runs_with_nobody_watching() {
    let studio = studio().await;
    let first = seq_of(&get(studio.addr, "/api/v1/frame").await.body);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let later = seq_of(&get(studio.addr, "/api/v1/frame").await.body);
    assert!(later > first, "the engine stopped: {first} -> {later}");
}

/// The UI is in the binary, one directory deep, and nothing else is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ui_is_served_from_the_binary() {
    let studio = studio().await;
    let at = studio.addr;

    let index = get(at, "/").await;
    assert_eq!(index.status, 200);
    assert!(String::from_utf8_lossy(&index.body).contains("Screeny Studio"));
    assert_eq!(get(at, "/main.js").await.status, 200);
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_socket_delivers_frames() {
    let studio = studio().await;
    let mut ws = Ws::connect(studio.addr, Some("watcher")).await;

    // The first thing a browser gets is the state, so it need not ask.
    let hello = ws.event("state").await;
    assert!(hello["state"]["piece"].is_string());

    let a = ws.frame().await;
    assert_eq!(a.len(), screeny_studio::engine::PACKET_BYTES);
    let b = ws.frame().await;
    assert!(seq_of(&b) > seq_of(&a), "frames did not advance: {} -> {}", seq_of(&a), seq_of(&b));

    // At 60 fps a second should bring a lot more than a handful.
    let n = ws.count_frames(Duration::from_secs(1)).await;
    assert!(n > 30, "only {n} frames in a second");
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

    // Alice changes the piece and the seed.
    post_as(at, "/api/v1/set_piece", r#"{"id":"plasma"}"#, Some("alice")).await;
    post_as(at, "/api/v1/set_seed", r#"{"seed":1234}"#, Some("alice")).await;

    // Bob is told, in order, and the event says who did it.
    let ev = bob.event("state").await;
    assert_eq!(ev["state"]["piece"], "plasma");
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
    post_as(at, "/api/v1/set_playback", r#"{"paused":true,"speed":1.0,"fps":30.0}"#, Some("bob")).await;
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
    post(at, "/api/v1/set_piece", r#"{"id":"plasma"}"#).await;
    post(at, "/api/v1/set_seed", r#"{"seed":77}"#).await;

    let mut late = Ws::connect(at, None).await;
    let hello = tokio::time::timeout(PATIENCE, late.event("state")).await.expect("a hello");
    assert_eq!(hello["state"]["piece"], "plasma");
    assert_eq!(hello["state"]["seed"], 77);
    assert_eq!(hello["from"], serde_json::Value::Null, "a resync is for everyone");
}

/// The heartbeat: twice a second, what the piece is performing and what the
/// panel link is doing. It is what replaced the UI's two polling timers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_socket_carries_the_heartbeat() {
    let studio = studio().await;
    post(studio.addr, "/api/v1/set_piece", r#"{"id":"clocks-numerals"}"#).await;
    let mut ws = Ws::connect(studio.addr, None).await;
    let status = ws.event("status").await;
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
