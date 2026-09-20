//! "Send to panel", through the HTTP API, into the reference receiver - and
//! the property the server is built around: a browser that has stopped reading
//! cannot slow the engine or the panel link down.
//!
//! The receiver is `screeny-sim` (card 006), the second, independent
//! implementation of the protocol, exactly as `crates/art/tests/sender.rs`
//! uses it for card 101's acceptance. This file is that acceptance again with
//! the studio's HTTP API in the middle of it.
//!
//! **No test in this file may touch the bench device.** Loopback only, a
//! fixed local port pair well away from the spec's, mDNS off, and every wait
//! has a deadline.

mod common;

use common::{get, post, preview_of, seq_of, studio, until, Ws};
use std::sync::mpsc::{channel, Receiver, Sender as Tx};
use std::time::Duration;

use screeny_sim::{Config, SimDevice, State};

/// One frame as the *device* saw it.
struct Shown {
    decoded: Vec<u8>,
    codec: u8,
    bytes: usize,
}

/// A simulator on loopback with **consecutive** ports, mDNS off.
///
/// Consecutive on purpose: an `IP:PORT` target resolves with the control port
/// taken to be frame + 1, which is true of the spec's defaults and never true
/// of an ephemeral pair. Card 101 hit the same thing and wrote it up as API
/// feedback; this is the same workaround, not a new one.
fn start_sim() -> (SimDevice, u16, Receiver<Shown>) {
    let (tx, rx): (Tx<Shown>, Receiver<Shown>) = channel();
    // Well above 49374/49375, so a stray packet cannot reach the bench device
    // even in principle.
    for port in 50_700u16..50_780 {
        let cfg = Config { frame_port: port, control_port: port + 1, ..Config::for_test() };
        let tx = tx.clone();
        let sink = Box::new(move |f: &screeny_proto::Rgb888Frame, m: &screeny_sim::FrameMeta| {
            let _ = tx.send(Shown { decoded: f.to_vec(), codec: m.codec, bytes: m.bytes });
        });
        if let Ok(dev) = SimDevice::start_with(cfg, Some(sink)) {
            return (dev, port, rx);
        }
    }
    panic!("no free consecutive port pair in 50700..50780");
}

/// Stop the piece and the limiter, so every frame the engine makes is the
/// same frame and "the panel shows the preview" is a statement about bytes
/// rather than about timing.
async fn hold_still(at: std::net::SocketAddr, piece: &str) {
    post(at, "/api/v1/set_piece", &format!(r#"{{"id":"{piece}"}}"#)).await;
    post(at, "/api/v1/set_seed", r#"{"seed":7}"#).await;
    let state = post(at, "/api/v1/set_playback", r#"{"paused":true,"speed":1.0,"fps":60.0}"#).await.json();
    let mut settings = state["settings"].clone();
    settings["limiter"]["enabled"] = false.into();
    post(at, "/api/v1/set_settings", &serde_json::json!({ "settings": settings }).to_string()).await;
}

/// Card 105's half of card 101's acceptance, which card 170 makes a tautology
/// and keeps anyway: the studio streams over the network, and what the device
/// puts up is what the browser is drawing.
///
/// It is a tautology now because there is one engine - the browser is handed
/// the same decoded datagram the panel is sent - and that is exactly why the
/// test is worth keeping: if the two ever come apart again, this says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_to_panel_streams_the_picture_to_the_device() {
    let (_dev, port, rx) = start_sim();
    let studio = studio().await;
    let at = studio.addr;
    hold_still(at, "plasma").await;

    // The switch, as the UI turns it on: a name or an address, deferred.
    let body = format!(r#"{{"on":true,"to":"127.0.0.1:{port}"}}"#);
    let first = post(at, "/api/v1/set_panel", &body).await;
    assert_eq!(first.status, 200, "{}", String::from_utf8_lossy(&first.body));
    assert_eq!(first.json()["on"], true, "the answer says what happened: {}", first.json());
    assert_eq!(first.json()["panel"]["target"], format!("127.0.0.1:{port}"));
    assert_eq!(first.json()["state"]["piece"], "plasma", "and what the page is showing");

    // The link is deferred, so it comes up on its own; the status route is
    // what the UI's status line reads.
    let mut status = serde_json::Value::Null;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        status = get(at, "/api/v1/panel_status").await.json();
        if status["connected"] == true && status["frames_sent"].as_u64().unwrap_or(0) > 5 {
            break;
        }
    }
    assert_eq!(status["connected"], true, "the link never came up: {status}");
    assert_eq!(status["state"], "up");
    assert!(status["device"].is_string(), "the link says which device it settled on: {status}");
    assert!(status["frames_sent"].as_u64().unwrap() > 5, "nothing reached the wire: {status}");
    assert_eq!(status["indexed_fallback"], 0, "plasma is an indexed piece: {status}");
    // Frames the engine offered while the deferred link was still finding the
    // device are counted as dropped, by design. Once it is up, nothing is.
    let dropped_while_connecting = status["frames_dropped"].as_u64().expect("a count");

    // What the browser is drawing.
    let mut ws = Ws::connect(at, None).await;
    let preview = preview_of(&ws.frame().await);

    // What the device is showing. Take the newest few: the first frames of a
    // run are sent while the meter is still learning the device's budget.
    let mut shown = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while shown.len() < 10 {
        let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else { break };
        match rx.recv_timeout(left) {
            Ok(s) => shown.push(s),
            Err(_) => break,
        }
    }
    assert!(shown.len() >= 10, "the device only saw {} frames", shown.len());

    let last = shown.last().expect("frames");
    assert_eq!(last.decoded.len(), 64 * 32 * 3);
    assert_eq!(
        last.decoded, preview,
        "the device is showing something other than what the browser is drawing \
         (codec {:#04x}, {} bytes)",
        last.codec, last.bytes
    );
    // And it is a still picture, so that comparison is not luck.
    for s in &shown[1..] {
        assert_eq!(s.decoded, last.decoded, "a paused piece sent two different frames");
    }
    let now = get(at, "/api/v1/panel_status").await.json();
    assert_eq!(
        now["frames_dropped"], dropped_while_connecting,
        "the link dropped a frame after it was up: {now}"
    );
    println!(
        "send_to_panel: {} offered, {} sent, {} coalesced, {} dropped before the link was up; \
         codec {:#04x}, {} B/frame, exact {}, fallback {}",
        now["frames_offered"],
        now["frames_sent"],
        now["frames_coalesced"],
        dropped_while_connecting,
        last.codec,
        last.bytes,
        now["indexed_exact"],
        now["indexed_fallback"],
    );

    // Turning it off drops the link, which sends FINAL and releases the panel.
    let off = post(at, "/api/v1/set_panel", r#"{"on":false,"to":""}"#).await;
    assert_eq!(off.json()["on"], false);
    assert_eq!(off.json()["panel"], serde_json::Value::Null);
    assert_eq!(get(at, "/api/v1/panel_status").await.json(), serde_json::Value::Null);
    // ...and the page carries on showing the piece, which is the half of this
    // that card 170 added.
    assert_eq!(off.json()["state"]["piece"], "plasma");
}

/// **`set_panel`, as another session drives it**, with the two exact bodies a
/// script uses to borrow the panel for firmware tests.
///
/// What is being pinned is not the status code. On the deployed service today
/// `{"on":false}` answers 200 and the panel keeps getting 30 fps, because since
/// card 106 that route only switched the *preview's* link while the device's
/// own player carried on - and a firmware conformance suite ran against a panel
/// it believed it had, for 35 false failures. So every assertion here is about
/// what the **device** sees.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn set_panel_hands_the_panel_over_and_takes_it_back() {
    let (dev, port, _rx) = start_sim();
    let studio = studio().await;
    let at = studio.addr;

    // A panel the studio is attached to and streaming to, the ordinary way.
    let added = post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{port}","name":"bench","play":true}}"#)).await;
    assert_eq!(added.status, 200, "{}", String::from_utf8_lossy(&added.body));
    until(Duration::from_secs(20), "the panel to be LIVE", || async {
        dev.handle().snapshot().state == State::Live
    })
    .await;

    // ---- {"on":false} : the panel is really let go. ----
    let off = post(at, "/api/v1/set_panel", r#"{"on":false}"#).await;
    assert_eq!(off.status, 200, "{}", String::from_utf8_lossy(&off.body));
    let body = off.json();
    assert_eq!(body["on"], false, "the answer says what happened rather than null: {body}");
    assert_eq!(body["panel"], serde_json::Value::Null, "the link is gone");
    assert!(body["state"]["piece"].is_string(), "and the page is still showing something: {body}");

    // The device: FINAL arrived, so it leaves LIVE and lets its source go.
    until(Duration::from_secs(10), "the panel to leave LIVE", || async {
        let s = dev.handle().snapshot();
        s.state != State::Live && s.active_source.is_none()
    })
    .await;

    // And **no more frames arrive**. This is the assertion the firmware
    // session needed and did not have.
    let settled = dev.handle().telemetry().frames_rx;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        dev.handle().telemetry().frames_rx,
        settled,
        "the studio is still sending to a panel it said it had let go"
    );

    // The status route agrees, and the studio is still well and still playing.
    let s = get(at, "/api/v1/status").await.json();
    assert_eq!(s["preview"]["panel_on"], false, "{}", s["preview"]);
    assert_eq!(s["preview"]["panel"], serde_json::Value::Null);
    assert_eq!(s["preview"]["alive"], true, "the picture carries on with the panel let go");
    assert_eq!(s["devices"][0]["player"]["on"], false);
    assert_eq!(s["devices"][0]["player"]["panel"], serde_json::Value::Null);
    assert_eq!(s["ok"], true, "letting the panel go is not a fault: {}", s["problems"]);
    assert_eq!(get(at, "/healthz").await.status, 200);

    // ---- {"on":true,"to":"..."} : and the studio takes it back. ----
    let on = post(at, "/api/v1/set_panel", &format!(r#"{{"on":true,"to":"127.0.0.1:{port}"}}"#)).await;
    assert_eq!(on.status, 200, "{}", String::from_utf8_lossy(&on.body));
    assert_eq!(on.json()["on"], true);
    until(Duration::from_secs(20), "the panel to be LIVE again", || async {
        dev.handle().snapshot().state == State::Live && dev.handle().telemetry().frames_rx > settled + 10
    })
    .await;
    println!(
        "set_panel: LIVE -> {} frames while released -> LIVE again with {} received",
        0,
        dev.handle().telemetry().frames_rx - settled
    );
}

/// A browser that has stopped reading must cost the engine and the panel
/// nothing. This is the property the whole design turns on: the studio is
/// meant to stream for months with nobody watching, and "nobody watching" has
/// to include "a tab that is wedged".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_browser_does_not_hold_up_the_engine_or_the_panel() {
    let (_dev, port, _rx) = start_sim();
    let studio = studio().await;
    let at = studio.addr;
    hold_still(at, "plasma").await;
    post(at, "/api/v1/set_panel", &format!(r#"{{"on":true,"to":"127.0.0.1:{port}"}}"#)).await;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if get(at, "/api/v1/panel_status").await.json()["connected"] == true {
            break;
        }
    }

    // A browser with a receive buffer too small for one frame, which then
    // stops reading entirely.
    let stalled = Ws::connect_on(tiny_socket(at).await, Some("stalled")).await;

    // Let it wedge.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let before_seq = seq_of(&get(at, "/api/v1/frame").await.body);
    let before_sent = get(at, "/api/v1/panel_status").await.json()["frames_sent"].as_u64().expect("a count");

    // A healthy browser, while the wedged one is still connected.
    let mut healthy = Ws::connect(at, Some("healthy")).await;
    let window = Duration::from_secs(2);
    let frames = healthy.count_frames(window).await;

    let after_seq = seq_of(&get(at, "/api/v1/frame").await.body);
    let after_sent = get(at, "/api/v1/panel_status").await.json()["frames_sent"].as_u64().expect("a count");

    let ticks = after_seq - before_seq;
    let sent = after_sent - before_sent;
    println!("stalled browser: engine {ticks} ticks, panel {sent} frames, healthy browser {frames} frames in 2 s");

    // 60 fps for two seconds. Generous bounds: this runs on a loaded bench.
    assert!(ticks > 60, "the engine slowed to {ticks} ticks in 2 s behind a stalled browser");
    assert!(sent > 30, "the panel link sent only {sent} frames in 2 s behind a stalled browser");
    assert!(frames > 30, "a healthy browser got only {frames} frames in 2 s beside a stalled one");
    drop(stalled);
}

/// And the wedged socket does not sit there for ever: one send that cannot
/// complete inside `ws::STALL` closes it. Nothing is queued on its behalf in
/// the meantime, so this is tidiness rather than safety - but a server that
/// runs for months should not accumulate sockets that nobody is reading.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_browser_is_eventually_dropped() {
    let studio = studio().await;
    let mut stalled = Ws::connect_on(tiny_socket(studio.addr).await, Some("stalled")).await;

    // Long enough for the socket to wedge and the 3 s timeout to expire.
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Now drain it. Everything buffered arrives, and then the server's close.
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stalled.next().await {
                Some(common::Msg::Close) | None => return true,
                Some(_) => {}
            }
        }
    })
    .await;
    assert_eq!(closed, Ok(true), "the studio kept feeding a browser that stopped reading");
}

/// A socket whose receive buffer is smaller than one frame packet: it wedges
/// as soon as the studio sends, without waiting for a megabyte of kernel
/// buffer to fill.
async fn tiny_socket(addr: std::net::SocketAddr) -> tokio::net::TcpStream {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).expect("a socket");
    socket.set_recv_buffer_size(2048).expect("a small receive buffer");
    socket.connect(&addr.into()).expect("connect to the studio");
    socket.set_nonblocking(true).expect("non-blocking");
    tokio::net::TcpStream::from_std(socket.into()).expect("a tokio socket")
}
