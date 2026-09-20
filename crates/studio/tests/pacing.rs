//! Card 196: what a parameter drag costs the *other* browser.
//!
//! The same question card 120 asked about frames, asked about state. Dragging
//! a slider is about sixty `POST /api/v1/set_param` a second and each one
//! broadcasts a full `StudioState` to every browser but the one that did it -
//! so with a phone and a laptop on the same page (card 170's normal case) the
//! phone is sent sixty JSON documents a second to draw one slider moving.
//!
//! Every test here is a **rate over a window**, and the bounds are wide: this
//! runs on a loaded bench, and card 093's lesson is that a timing test must
//! assert on the pacer's own schedule, never on the OS scheduler. The numbers
//! that would mean the feature had broken are far outside every bound below.
//!
//! The sockets here ask for `fps=0`, so what is measured is state and the
//! half-second heartbeat and nothing else: frames are card 120's business and
//! would drown the thing being weighed.

mod common;

use common::{get, post, post_as, studio, Msg, Ws};
use std::net::SocketAddr;
use std::time::Duration;

/// A drag, in `POST /api/v1/set_param` a second: what a mouse or a finger on a
/// slider actually produces.
const DRAG_HZ: u64 = 60;

/// What one browser was told over a window.
#[derive(Default)]
struct Seen {
    states: usize,
    state_bytes: usize,
    heartbeats: usize,
    /// The last state message, which is what the browser is left resting on.
    last: Option<serde_json::Value>,
    window: Duration,
}

impl Seen {
    fn per_s(&self) -> f64 {
        self.states as f64 / self.window.as_secs_f64().max(f64::EPSILON)
    }

    fn bytes_per_s(&self) -> f64 {
        self.state_bytes as f64 / self.window.as_secs_f64().max(f64::EPSILON)
    }
}

/// Everything a socket is told for `window`, sorted into state and heartbeat.
async fn watch(ws: &mut Ws, window: Duration) -> Seen {
    let deadline = tokio::time::Instant::now() + window;
    let mut seen = Seen { window, ..Seen::default() };
    while let Some(left) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        let Ok(next) = tokio::time::timeout(left, ws.next()).await else { break };
        match next {
            Some(Msg::Text(t)) => {
                let v: serde_json::Value = serde_json::from_str(&t).expect("valid JSON");
                if v["type"] == "state" {
                    seen.states += 1;
                    seen.state_bytes += t.len();
                    seen.last = Some(v);
                } else {
                    seen.heartbeats += 1;
                }
            }
            Some(_) => {}
            None => break,
        }
    }
    seen
}

/// What the studio says it has written to browsers so far (card 120).
async fn sockets_bytes(at: SocketAddr) -> u64 {
    get(at, "/api/v1/status").await.json()["sockets"]["bytes_sent"].as_u64().expect("a byte count")
}

/// A slider dragged for `how_long` by the browser called `who`, at [`DRAG_HZ`].
/// Returns how many changes were actually made and the value it finished on.
async fn drag(at: SocketAddr, who: &'static str, how_long: Duration) -> (usize, f32) {
    let mut ticker = tokio::time::interval(Duration::from_micros(1_000_000 / DRAG_HZ));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let deadline = tokio::time::Instant::now() + how_long;
    let (mut n, mut value) = (0usize, 0.0f32);
    while tokio::time::Instant::now() < deadline {
        ticker.tick().await;
        // A real drag never sends the same value twice in a row; nor does this.
        value = (n % 360) as f32;
        let body = format!(r#"{{"id":"hue","value":{value}}}"#);
        assert_eq!(post_as(at, "/api/v1/set_param", &body, Some(who)).await.status, 200);
        n += 1;
    }
    (n, value)
}

/// Two browsers on the same page, one of them dragging a slider: what does it
/// cost the other one?
///
/// This is the card's measurement, kept as a test so the answer cannot quietly
/// go back to what it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drag_costs_the_second_browser_a_bounded_number_of_messages() {
    let studio = studio().await;
    let at = studio.addr;
    assert_eq!(post(at, "/api/v1/set_patch", r#"{"id":"plasma"}"#).await.status, 200);

    // Two tabs, neither of them asking for pictures: this weighs state only.
    let mut dragger = Ws::connect_asking(at, "client=dragger&fps=0").await;
    let mut watcher = Ws::connect_asking(at, "client=watcher&fps=0").await;
    dragger.event("state").await; // the hello each of them is sent
    watcher.event("state").await;

    let window = Duration::from_secs(3);
    let before = sockets_bytes(at).await;
    let (drag, seen, own) = tokio::join!(
        drag(at, "dragger", window),
        watch(&mut watcher, window),
        watch(&mut dragger, window)
    );
    let (posts, ended_on) = drag;
    // Card 120's `sockets.bytes_sent` counts every message, state included -
    // it is incremented wherever a socket writes - so it is a fair instrument
    // for this card too, as long as nothing is asking for frames.
    let server_side = sockets_bytes(at).await - before;

    println!(
        "drag of {posts} changes in {:?}: the second browser saw {} state messages ({:.1}/s, {:.0} B/s), \
         {} heartbeats; the dragging browser saw {} state messages; the server counted {server_side} B \
         written to all sockets ({:.0} B/s)",
        window,
        seen.states,
        seen.per_s(),
        seen.bytes_per_s(),
        seen.heartbeats,
        own.states,
        server_side as f64 / window.as_secs_f64()
    );

    assert!(posts > 100, "the drag itself did not keep up: {posts} changes in {window:?}");
    assert_eq!(own.states, 0, "the dragging browser was told about its own drag {} times", own.states);
    assert!(seen.states > 0, "the second browser was told nothing at all about the drag");
    // The pacer's own schedule, not the scheduler's: one message every 50 ms
    // is 20 a second, so three seconds cannot hold more than 60 of them
    // however the bench is loaded. The bound is that arithmetic plus a
    // message's worth of slack at each end.
    assert!(
        seen.per_s() <= 22.0,
        "a drag still cost the second browser {:.1} state messages a second ({} in {window:?})",
        seen.per_s(),
        seen.states
    );
    assert!(
        seen.states * 2 < posts,
        "{} state messages for {posts} changes is not a saving",
        seen.states
    );
    assert!(
        server_side >= seen.state_bytes as u64,
        "sockets.bytes_sent ({server_side}) counted less than the one browser received ({})",
        seen.state_bytes
    );
    // What the browser is left resting on is the value the drag ended on: a
    // paced stream that drops the last message leaves the other tab wrong.
    let last = seen.last.expect("a state message");
    let hue = last["state"]["params"]["hue"].as_f64().expect("the hue parameter");
    println!("the second browser's last message: rev {}, hue {hue} (the drag ended on {ended_on})", last["rev"]);

    studio.stop().await;
}

/// A deliberate change - a patch picked, a switch flipped - is not a burst, and
/// must still feel instant on the other browser. The pacer's leading edge is
/// what makes that true: after a quiet moment there is nothing held, so the
/// change goes out where it stands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_change_still_arrives_at_once() {
    let studio = studio().await;
    let at = studio.addr;
    assert_eq!(post(at, "/api/v1/set_patch", r#"{"id":"plasma"}"#).await.status, 200);
    let mut watcher = Ws::connect_asking(at, "client=watcher&fps=0").await;
    watcher.event("state").await; // the hello

    // Ten changes, each after a moment of quiet, which is what a person
    // clicking things produces. The first one is thrown away: the studio is
    // still starting the patch, and what is being measured is the steady case.
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    for i in 0..10u32 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let value = f64::from(10 + i);
        let sent = tokio::time::Instant::now();
        let body = format!(r#"{{"id":"hue","value":{value}}}"#);
        assert_eq!(post_as(at, "/api/v1/set_param", &body, Some("dragger")).await.status, 200);
        let ev = watcher.event("state").await;
        let took = sent.elapsed();
        assert_eq!(ev["state"]["params"]["hue"], value, "the change that arrived was not the one made");
        if i > 0 {
            worst = worst.max(took);
            total += took;
        }
    }
    println!("a single change reached the second browser in {:?} at worst, {:?} on average", worst, total / 9);

    // The pacer's own schedule says zero: a leading edge holds nothing. The
    // bound is loose because an HTTP round trip and two task wake-ups on a
    // loaded bench are not, and because failing here must mean "it was held",
    // which would cost `STATE_GAP` (50 ms) at least and a whole burst's worth
    // at worst.
    assert!(worst < Duration::from_millis(250), "a single change took {worst:?} to reach the other browser");
    studio.stop().await;
}

/// The end of a drag is the value the other browser is left resting on, so the
/// last change of a burst is **held, not dropped**: it arrives within one gap
/// of the drag stopping. This is the difference between pacing state and
/// pacing frames, where a frame that is not due is thrown away.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_value_a_drag_ended_on_always_arrives() {
    let studio = studio().await;
    let at = studio.addr;
    assert_eq!(post(at, "/api/v1/set_patch", r#"{"id":"plasma"}"#).await.status, 200);
    let mut watcher = Ws::connect_asking(at, "client=watcher&fps=0").await;
    watcher.event("state").await; // the hello

    // Three drags in a row, each ending somewhere else: one lucky pass proves
    // nothing about a trailing edge.
    for round in 0..3 {
        let (posts, ended_on) = drag(at, "dragger", Duration::from_millis(700)).await;
        let stopped = tokio::time::Instant::now();
        // Everything the drag produced is read through until the value it
        // ended on turns up; `event` gives up after PATIENCE rather than hang.
        let mut seen = 0;
        let took = loop {
            let ev = watcher.event("state").await;
            seen += 1;
            if (ev["state"]["params"]["hue"].as_f64().expect("the hue parameter") - f64::from(ended_on)).abs()
                < f64::EPSILON
            {
                break stopped.elapsed();
            }
            assert!(seen < 200, "the drag's last value never arrived after {seen} state messages");
        };
        println!("round {round}: {posts} changes ending on hue {ended_on}; it arrived {took:?} after the drag stopped");
        // One gap (50 ms) is the pacer's answer; the bound is loose for the
        // same reasons as above, and is nowhere near "it was dropped", which
        // has no answer at all and would time out.
        assert!(took < Duration::from_millis(500), "the drag's last value took {took:?} to arrive");
    }
    studio.stop().await;
}
