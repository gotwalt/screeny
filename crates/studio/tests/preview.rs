//! Card 120: a browser is sent as many preview frames as it can use, and no
//! more.
//!
//! Every test here is a **rate over a window**, never a snapshot: the question
//! is how much a tab costs per second, and that has no answer at an instant.
//! The bounds are wide on purpose - this runs on a loaded bench - and every one
//! of them is far from the number that would mean the feature had broken.
//!
//! What must stay true, and is checked in `panel.rs` rather than here: none of
//! this can slow the render loop or the panel link down. Pacing is done by
//! dropping a frame where it stands, which is exactly what already happened to
//! a browser too slow to read one.

mod common;

use common::{get, post, studio, until, until_json, Ws, PATIENCE};
use std::time::Duration;

/// One frame packet: 52 bytes of header and 64x32 sRGB.
const PACKET: usize = screeny_studio::page::PACKET_BYTES;

/// A window long enough to average a rate over without the test being slow.
const WINDOW: Duration = Duration::from_secs(2);

/// Put the page on a patch, with nothing else moving.
///
/// Card 161 took the rate out of this: there is one, `screeny_art::FPS`, and
/// `/api/v1/status` reports it rather than being set to it. What is still
/// worth waiting for is a render loop that is actually running.
async fn playing(at: std::net::SocketAddr, patch: &str, paused: bool) {
    post(at, "/api/v1/set_patch", &format!(r#"{{"id":"{patch}"}}"#)).await;
    let body = format!(r#"{{"paused":{paused},"speed":1.0}}"#);
    assert_eq!(post(at, "/api/v1/set_playback", &body).await.status, 200);
    until_json(at, PATIENCE, "the player to be running", "/api/v1/status", |s| {
        s["preview"]["fps"] == screeny_art::FPS && s["preview"]["alive"] == true
    })
    .await;
}

/// The heart of it: a tab that says it is hidden is sent no pictures at all,
/// keeps its heartbeat, and is drawing again within a moment of coming back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hidden_tab_is_sent_no_frames_and_comes_straight_back() {
    let studio = studio().await;
    let at = studio.addr;
    playing(at, "plasma", false).await;

    let mut tab = Ws::connect_asking(at, "client=tab&fps=30").await;
    let visible = tab.measure(WINDOW).await;
    assert!(visible.frames > 20, "a visible tab asking for 30 fps got {} frames in 2 s", visible.frames);

    // The tab is switched away from.
    tab.say(r#"{"type":"preview","fps":0}"#).await;
    // Whatever was already in flight arrives; then nothing.
    let settling = tab.measure(Duration::from_millis(500)).await;
    let hidden = tab.measure(WINDOW).await;
    assert_eq!(hidden.frames, 0, "a hidden tab was sent {} frames ({} settling)", hidden.frames, settling.frames);
    assert!(hidden.texts > 0, "a hidden tab stopped getting its heartbeat");
    assert!(hidden.bytes_per_s() < 2000.0, "a hidden tab still cost {:.0} B/s", hidden.bytes_per_s());

    // And back.
    tab.say(r#"{"type":"preview","fps":30}"#).await;
    let back = tab.measure(WINDOW).await;
    assert!(back.frames > 20, "a tab that came back got {} frames in 2 s", back.frames);

    println!(
        "hidden tab: visible {:.0} B/s ({:.0} fps) -> hidden {:.0} B/s (0 fps, {} heartbeats) -> back {:.0} B/s",
        visible.bytes_per_s(),
        visible.fps(),
        hidden.bytes_per_s(),
        hidden.texts,
        back.bytes_per_s()
    );
    studio.stop().await;
}

/// A socket is paced to what it asked for, and a socket that asks for nothing
/// gets the default rather than everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_socket_gets_the_rate_it_asked_for() {
    let studio = studio().await;
    let at = studio.addr;
    playing(at, "plasma", false).await;

    let mut slow = Ws::connect_asking(at, "client=slow&fps=10").await;
    let mut quiet = Ws::connect_asking(at, "client=quiet").await;
    // Card 161: 60 is above the render rate, so it can only mean "everything",
    // which is what the default is too. Both are the full rate now.
    let mut fast = Ws::connect_asking(at, "client=fast&fps=60").await;

    // All three at once, so they are measured over the same window and a slow
    // one cannot be blamed on the bench.
    let (slow, quiet, fast) =
        tokio::join!(slow.measure(WINDOW), quiet.measure(WINDOW), fast.measure(WINDOW));

    println!(
        "asked 10 -> {:.1} fps, {:.0} B/s; asked nothing -> {:.1} fps, {:.0} B/s; asked 60 -> {:.1} fps, {:.0} B/s",
        slow.fps(),
        slow.bytes_per_s(),
        quiet.fps(),
        quiet.bytes_per_s(),
        fast.fps(),
        fast.bytes_per_s()
    );

    assert!((5.0..15.0).contains(&slow.fps()), "a socket asking for 10 fps got {:.1}", slow.fps());
    assert!(quiet.fps() > slow.fps() * 1.5, "a socket that asked for nothing got {:.1} fps", quiet.fps());
    assert!(quiet.fps() <= screeny_art::FPS * 1.2, "and no more than there is: {:.1} fps", quiet.fps());
    assert!(
        fast.fps() > slow.fps() * 1.5,
        "asking for more than is rendered got {:.1} fps, no more than the slow one",
        fast.fps()
    );
    assert_eq!(slow.frame_bytes / slow.frames.max(1), PACKET, "the frames are the panel's own, unaltered");
    studio.stop().await;
}

/// A picture that has not changed is not sent again - but not never: the page's
/// meters are read out of the frame header, so an unchanging picture still says
/// so about once a second.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_picture_that_has_not_changed_is_not_sent_again() {
    let studio = studio().await;
    let at = studio.addr;
    // Paused: the strongest form of "a held clock face", and the same thing a
    // numerals clock does for fifteen seconds between minutes.
    playing(at, "clocks-numerals", true).await;

    let mut plain = Ws::connect_asking(at, "client=plain&fps=30").await;
    let mut lean = Ws::connect_asking(at, "client=lean&fps=30&repeat=false").await;
    let window = Duration::from_secs(3);
    let (plain, lean) = tokio::join!(plain.measure(window), lean.measure(window));

    println!(
        "a still picture: repeat on -> {:.1} fps, {:.0} B/s; repeat off -> {:.1} fps, {:.0} B/s",
        plain.fps(),
        plain.bytes_per_s(),
        lean.fps(),
        lean.bytes_per_s()
    );
    assert!(plain.fps() > 20.0, "the unchanged default stopped sending: {:.1} fps", plain.fps());
    assert!(lean.frames > 0, "an unchanging picture must still be refreshed, for the meters");
    assert!(lean.fps() < 3.0, "repeat=false still sent {:.1} fps of the same picture", lean.fps());
    studio.stop().await;
}

/// **A hidden tab is not a watcher.** A studio with no panel and only hidden
/// tabs open renders at `player::IDLE_FPS`, exactly as if nobody were connected
/// at all - and picks up again within a moment of somebody looking.
///
/// This is the half of card 120 that is not about bytes: a phone left on the
/// page in a pocket must not hold a core at the full rate for a month.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hidden_tab_does_not_hold_the_player_at_full_rate() {
    let studio = studio().await;
    let at = studio.addr;
    // No panel is ever attached here, so the only reason to render fast would
    // be a browser watching.
    playing(at, "plasma", false).await;

    // Rendered frames over a window, read from the sequence number the render
    // loop stamps - the player's own rate, not the socket's.
    async fn rendered(at: std::net::SocketAddr, window: Duration) -> f64 {
        let before = common::seq_of(&get(at, "/api/v1/frame").await.body);
        tokio::time::sleep(window).await;
        let after = common::seq_of(&get(at, "/api/v1/frame").await.body);
        f64::from(after.wrapping_sub(before)) / window.as_secs_f64()
    }

    let mut tab = Ws::connect_asking(at, "client=tab&fps=30").await;
    // Let the socket be counted before measuring.
    until(PATIENCE, "the studio to count a watcher", || async {
        get(at, "/api/v1/status").await.json()["sockets"]["watching"] == 1
    })
    .await;
    let watched = rendered(at, WINDOW).await;

    tab.say(r#"{"type":"preview","fps":0}"#).await;
    until(PATIENCE, "the studio to stop counting it", || async {
        let s = get(at, "/api/v1/status").await.json();
        s["sockets"]["watching"] == 0 && s["sockets"]["open"] == 1
    })
    .await;
    let unwatched = rendered(at, WINDOW).await;

    tab.say(r#"{"type":"preview","fps":30}"#).await;
    until(PATIENCE, "the studio to count it again", || async {
        get(at, "/api/v1/status").await.json()["sockets"]["watching"] == 1
    })
    .await;
    let again = rendered(at, WINDOW).await;

    println!("no panel: watched {watched:.0} fps -> hidden {unwatched:.0} fps -> watched again {again:.0} fps");
    // Card 161: the full rate is 30, so these bounds sit between IDLE_FPS (5)
    // and it rather than above 30.
    assert!(watched > 18.0, "a watched player with no panel should render at its rate, not {watched:.0} fps");
    assert!(unwatched < 12.0, "a hidden tab held the player at {unwatched:.0} fps");
    assert!(again > 18.0, "the player did not pick up again when the tab came back: {again:.0} fps");

    // And a socket that simply goes away gives its claim back too.
    drop(tab);
    until(PATIENCE, "the socket to be forgotten", || async {
        get(at, "/api/v1/status").await.json()["sockets"]["open"] == 0
    })
    .await;
    studio.stop().await;
}

/// The counters `/api/v1/status` grew are the ones the card asked for, and they
/// only ever go up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_status_route_says_what_the_preview_costs() {
    let studio = studio().await;
    let at = studio.addr;
    playing(at, "plasma", false).await;

    let idle = get(at, "/api/v1/status").await.json();
    assert_eq!(idle["sockets"]["open"], 0);
    assert_eq!(idle["sockets"]["watching"], 0);
    let before = idle["sockets"]["bytes_sent"].as_u64().expect("a byte count");

    let mut tab = Ws::connect_asking(at, "client=tab&fps=30").await;
    let seen = tab.measure(WINDOW).await;
    let after = get(at, "/api/v1/status").await.json();
    let sent = after["sockets"]["bytes_sent"].as_u64().expect("a byte count") - before;
    let frames = after["sockets"]["frames_sent"].as_u64().expect("a frame count");

    println!("server-side: {sent} bytes, {frames} frame packets; the browser saw {} bytes", seen.frame_bytes + seen.text_bytes);
    assert!(sent >= (seen.frame_bytes + seen.text_bytes) as u64, "the server counted less than the browser received");
    assert!(frames >= seen.frames as u64);
    studio.stop().await;
}
