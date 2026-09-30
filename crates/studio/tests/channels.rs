//! Card 350's acceptance: **several panels**, end to end, against two
//! `screeny-sim` panels on loopback.
//!
//! One picture on two panels is one channel - one render per tick, the same
//! frames on both devices, in sync. Different pictures are different
//! channels. "Same as" puts them back together. A route without `panel` is the
//! first panel's; `set_panel {"on":false}` without one lets every panel go. A
//! socket can be scoped to one panel, cheaply. A device the registry learns
//! about becomes a panel, idle.
//!
//! **No test in this file may touch the bench device.** Loopback only, a port
//! band well away from the spec's, mDNS off, and every wait has a deadline.

mod common;

use common::{get, post, studio_and_state, until_json, Ws, PATIENCE};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use screeny_sim::{Config as SimConfig, SimDevice};

/// The port band these tests use. Well above 49374/49375, and apart from the
/// bands the other test files use.
const FIRST_PORT: u16 = 51_000;
const LAST_PORT: u16 = 51_100;

/// What one simulated panel has put up, newest last, bounded.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<Vec<u8>>>>);

impl Seen {
    fn count(&self) -> usize {
        self.0.lock().expect("seen").len()
    }

    /// The frames that arrived after `from`.
    fn since(&self, from: usize) -> Vec<Vec<u8>> {
        self.0.lock().expect("seen")[from..].to_vec()
    }
}

/// A simulator with its own id on **consecutive** loopback ports (an
/// `IP:PORT` a human types resolves with the control port taken to be frame +
/// 1), recording every frame it shows.
fn start_sim(id: &str, avoid: u16) -> (SimDevice, u16, Seen) {
    let seen = Seen::default();
    for port in (FIRST_PORT..LAST_PORT).step_by(2) {
        if port == avoid {
            continue;
        }
        let cfg = SimConfig { frame_port: port, control_port: port + 1, id: id.to_string(), instance: format!("sim-{id}"), ..SimConfig::for_test() };
        let log = seen.clone();
        let sink = Box::new(move |f: &screeny_proto::Rgb888Frame, _: &screeny_sim::FrameMeta| {
            let mut v = log.0.lock().expect("seen");
            if v.len() < 20_000 {
                v.push(f.to_vec());
            }
        });
        if let Ok(dev) = SimDevice::start_with(cfg, Some(sink)) {
            return (dev, port, seen);
        }
    }
    panic!("no free consecutive port pair in {FIRST_PORT}..{LAST_PORT}");
}

async fn ok(at: SocketAddr, path: &str, body: &str) -> serde_json::Value {
    let r = post(at, path, body).await;
    assert_eq!(r.status, 200, "{path} {body}: {}", String::from_utf8_lossy(&r.body));
    r.json()
}

/// The overview's card for one panel.
fn card<'a>(panels: &'a serde_json::Value, device: &str) -> &'a serde_json::Value {
    panels["panels"].as_array().expect("panels").iter().find(|p| p["device"] == device).unwrap_or_else(|| panic!("no `{device}` in {panels}"))
}

/// How many of `a`'s frames `b` also showed, byte for byte, as a fraction.
fn overlap(a: &[Vec<u8>], b: &[Vec<u8>]) -> f64 {
    let bs: HashSet<&Vec<u8>> = b.iter().collect();
    let shared = a.iter().filter(|f| bs.contains(f)).count();
    shared as f64 / a.len().max(1) as f64
}

/// For each of `a`'s frames, how far the nearest of `b`'s is - the mean
/// absolute difference per byte - and the median of those.
///
/// Byte-for-byte equality is too strict a test of "the same picture" on two
/// panels whose histories differ: each panel has its own output stage, and
/// its encoder prefers the codec it used last (a small hysteresis), so the
/// same linear frame can reach two devices through two different lossy
/// codecs. This is the looser question: is every frame one panel shows
/// *nearly* one the other showed?
fn nearest(a: &[Vec<u8>], b: &[Vec<u8>]) -> f64 {
    let mut d: Vec<f64> = a
        .iter()
        .map(|fa| {
            b.iter()
                .map(|fb| fa.iter().zip(fb).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / fa.len().max(1) as f64)
                .fold(f64::INFINITY, f64::min)
        })
        .collect();
    d.sort_by(f64::total_cmp);
    d.get(d.len() / 2).copied().unwrap_or(f64::INFINITY)
}

/// Both panels streaming, limiter off on both (so each panel's output stage is
/// a pure function of the frame it is handed), on a moving patch.
async fn two_panels(at: SocketAddr, a_port: u16, b_port: u16) {
    for port in [a_port, b_port] {
        ok(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{port}","play":true}}"#)).await;
    }
    until_json(at, PATIENCE * 2, "both panels known by their own ids", "/api/v1/panels", |p| {
        let ids: Vec<&str> = p["panels"].as_array().map(|a| a.iter().filter_map(|x| x["device"].as_str()).collect()).unwrap_or_default();
        ids == ["aa0001", "bb0002"]
    })
    .await;
    for panel in ["aa0001", "bb0002"] {
        let state = ok(at, "/api/v1/set_patch", &format!(r#"{{"id":"metaballs","panel":"{panel}"}}"#)).await;
        let mut output = state["output"].clone();
        output["limiter"]["enabled"] = false.into();
        ok(at, "/api/v1/set_output", &serde_json::json!({ "output": output, "panel": panel }).to_string()).await;
    }
}

/// Wait out a channel change's fade, and give both sims time to show frames.
async fn settle() {
    tokio::time::sleep(Duration::from_secs_f32(screeny_studio::channel::FADE_MANUAL + 1.0)).await;
}

/// A second of what both sims showed.
async fn a_second_of(a: &Seen, b: &Seen) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let (fa, fb) = (a.count(), b.count());
    tokio::time::sleep(Duration::from_secs(1)).await;
    (a.since(fa), b.since(fb))
}

/// **The card's acceptance**: the same picture on two panels, then different
/// pictures, then "same as" - and each sim shows what it should.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_panels_share_a_picture_then_differ_then_share_again() {
    let (_a_dev, a_port, a) = start_sim("aa0001", 0);
    let (_b_dev, b_port, b) = start_sim("bb0002", a_port);
    let (studio, st) = studio_and_state().await;
    let at = studio.addr;
    two_panels(at, a_port, b_port).await;

    // 1. The same picture: rule 1 put the second panel on the first one's
    //    channel. One channel, one render per tick handed to both panels,
    //    and both sims show the same picture.
    settle().await;
    let panels = get(at, "/api/v1/panels").await.json();
    let (ca, cb) = (card(&panels, "aa0001"), card(&panels, "bb0002"));
    assert_eq!(ca["channel"], cb["channel"], "one picture, one channel: {panels}");
    assert_eq!(ca["shared_with"], serde_json::json!(["bb0002"]));
    assert_eq!((ca["connected"].as_bool(), cb["connected"].as_bool()), (Some(true), Some(true)), "{panels}");
    let channels = st.panels.channels();
    assert_eq!(channels.len(), 1, "one render for both");
    let pa = st.panels.get("aa0001").expect("a");
    let pb = st.panels.get("bb0002").expect("b");
    let (t0, a0, b0) = (channels[0].ticks(), pa.presents(), pb.presents());
    let (fa, fb) = a_second_of(&a, &b).await;
    let (t1, a1, b1) = (channels[0].ticks(), pa.presents(), pb.presents());
    let renders = t1 - t0;
    assert!(renders >= 20, "the channel renders at the full rate while both are connected: {renders}");
    assert!((a1 - a0).abs_diff(renders) <= 1 && (b1 - b0).abs_diff(renders) <= 1, "every render reaches both panels: {renders} renders, {} and {} presents", a1 - a0, b1 - b0);
    let same = overlap(&fa, &fb);
    eprintln!(
        "same picture: {renders} renders in 1 s; sim a showed {}, sim b {}; {:.0}% of a's frames also on b, byte for byte; nearest {:.2}",
        fa.len(),
        fb.len(),
        same * 100.0,
        nearest(&fa, &fb)
    );
    assert!(fa.len() >= 20 && fb.len() >= 20, "both panels are streaming: {} and {}", fa.len(), fb.len());
    // The same picture on both devices, to within what two independently
    // chosen lossy codecs make of one frame (measured 0.0-1.2; two different
    // patches measure ~50).
    assert!(nearest(&fa, &fb) < 3.0, "the same picture on both panels: {:.2}", nearest(&fa, &fb));

    // 2. Different pictures: the second panel splits off (rule 3); two
    //    channels, each rendering, and the sims no longer agree.
    let dials = ok(at, "/api/v1/set_patch", r#"{"id":"clocks-dials","panel":"bb0002"}"#).await;
    assert_eq!((dials["patch"].as_str(), dials["shared_with"].as_array().map(Vec::len)), (Some("clocks-dials"), Some(0)));
    settle().await;
    let panels = get(at, "/api/v1/panels").await.json();
    assert_ne!(card(&panels, "aa0001")["channel"], card(&panels, "bb0002")["channel"], "{panels}");
    assert_eq!(card(&panels, "aa0001")["picture"]["patch"], "metaballs", "the first is where it was");
    assert_eq!(st.panels.channels().len(), 2, "two pictures, two renders");
    let (fa, fb) = a_second_of(&a, &b).await;
    let same = overlap(&fa, &fb);
    eprintln!("different pictures: sim a showed {}, sim b {}; {:.0}% shared; nearest {:.2}", fa.len(), fb.len(), same * 100.0, nearest(&fa, &fb));
    assert!(fa.len() >= 20 && fb.len() >= 1, "both still streaming: {} and {}", fa.len(), fb.len());
    assert!(nearest(&fa, &fb) > 10.0, "two different pictures: {:.2}", nearest(&fa, &fb));

    // A route without `panel` is the first panel's.
    let seeded = ok(at, "/api/v1/set_seed", r#"{"seed":4242}"#).await;
    assert_eq!(seeded["device"], "aa0001", "the first panel: {seeded}");
    let b_state = get(at, "/api/v1/bootstrap?panel=bb0002").await.json();
    assert_ne!(b_state["state"]["seed"], 4242, "and not the second: {b_state}");

    // 3. "Same as": the second panel joins the first's channel, tweaks (the
    //    seed) and all.
    let joined = ok(at, "/api/v1/same_as", r#"{"panel":"bb0002","as":"aa0001"}"#).await;
    assert_eq!((joined["patch"].as_str(), joined["seed"].as_u64()), (Some("metaballs"), Some(4242)));
    assert_eq!(joined["modified"], true, "a tweaked picture, honestly marked");
    settle().await;
    assert_eq!(st.panels.channels().len(), 1, "the channel it left is dropped once its fade is over");
    let (fa, fb) = a_second_of(&a, &b).await;
    let same = overlap(&fa, &fb);
    eprintln!("same as: sim a showed {}, sim b {}; {:.0}% of a's frames also on b; nearest {:.2}", fa.len(), fb.len(), same * 100.0, nearest(&fa, &fb));
    assert!(fa.len() >= 20 && fb.len() >= 20, "both panels are streaming: {} and {}", fa.len(), fb.len());
    assert!(nearest(&fa, &fb) < 3.0, "the same picture again: {:.2}", nearest(&fa, &fb));

    // Detach: a copy of its own; an edit to it does not reach the first.
    let detached = ok(at, "/api/v1/detach", r#"{"panel":"bb0002"}"#).await;
    assert_eq!(detached["shared_with"], serde_json::json!([]));
    assert_eq!(detached["seed"], 4242, "the same working copy");
    ok(at, "/api/v1/set_seed", r#"{"seed":7,"panel":"bb0002"}"#).await;
    let first = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(first["state"]["seed"], 4242, "the edit stayed on the detached panel");

    // `set_panel {"on":false}` without a panel lets every panel go.
    let off = ok(at, "/api/v1/set_panel", r#"{"on":false}"#).await;
    assert_eq!(off["on"], false);
    let panels = until_json(at, PATIENCE, "both panels let go", "/api/v1/panels", |p| {
        p["panels"].as_array().is_some_and(|a| a.iter().all(|x| x["link"] == "off"))
    })
    .await;
    assert!(panels["panels"].as_array().expect("panels").iter().all(|x| x["connected"] == false));
    let (fa, fb) = a_second_of(&a, &b).await;
    assert!(fa.len() <= 1 && fb.len() <= 1, "no more frames once let go: {} and {}", fa.len(), fb.len());
    studio.stop().await;
}

/// A socket scoped to one panel is sent that panel's state and frames, at the
/// rate it asked for - which is how the overview's thumbnails are fed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_socket_can_be_about_one_panel() {
    let (_a_dev, a_port, _a) = start_sim("aa0001", 0);
    let (_b_dev, b_port, _b) = start_sim("bb0002", a_port);
    let (studio, _st) = studio_and_state().await;
    let at = studio.addr;
    two_panels(at, a_port, b_port).await;
    ok(at, "/api/v1/set_patch", r#"{"id":"clocks-dials","panel":"bb0002"}"#).await;

    let mut thumb = Ws::connect_asking(at, "panel=bb0002&fps=4&overview=false").await;
    let hello = thumb.event("state").await;
    assert_eq!(hello["panel"], "bb0002");
    assert_eq!(hello["state"]["patch"], "clocks-dials", "{hello}");
    thumb.frame().await;
    let t = thumb.measure(Duration::from_secs(2)).await;
    assert!((5..=10).contains(&t.frames), "about four frames a second: {} in 2 s", t.frames);

    // A change to that panel reaches it; one to the other panel's picture
    // arrives as a state message that is still about this one.
    ok(at, "/api/v1/set_seed", r#"{"seed":99,"panel":"bb0002"}"#).await;
    let ev = thumb.event("state").await;
    assert_eq!((ev["panel"].as_str(), ev["state"]["seed"].as_u64()), (Some("bb0002"), Some(99)), "{ev}");

    // The page's socket, without `panel`, is the first panel's and is sent the
    // overview.
    let mut page = Ws::connect_asking(at, "fps=0").await;
    let hello = page.event("state").await;
    assert_eq!(hello["panel"], "aa0001");
    let overview = page.event("panels").await;
    assert_eq!(overview["panels"].as_array().map(Vec::len), Some(2), "{overview}");

    // A socket about a panel that is not there is told so and closed.
    let mut nobody = Ws::connect_asking(at, "panel=nope").await;
    let err = nobody.event("error").await;
    assert!(err["error"].as_str().is_some_and(|e| e.contains("nope")), "{err}");
    studio.stop().await;
}

/// **Auto-adopt**: a device the registry learns about - here added straight
/// to it, as a browse would - becomes a panel within a supervisor tick, idle:
/// no channel, no link, and the studio's first picture is not handed to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_the_registry_learns_about_becomes_an_idle_panel() {
    let (studio, st) = studio_and_state().await;
    let at = studio.addr;
    let before = get(at, "/api/v1/panels").await.json();
    assert_eq!(before["panels"][0]["unbound"], true, "a fresh studio shows its picture on the stand-in: {before}");

    // Nothing listens on port 9 of loopback: the device is known, never heard.
    let id = st.devices.add_manual("127.0.0.1:9", "").expect("added");
    let panels = until_json(at, PATIENCE, "the device adopted as a panel", "/api/v1/panels", |p| {
        p["panels"].as_array().is_some_and(|a| a.iter().any(|x| x["device"] == id))
    })
    .await;
    let p = card(&panels, &id);
    assert_eq!(p["link"], "idle", "{panels}");
    assert_eq!(p["channel"], serde_json::Value::Null);
    assert_eq!(p["picture"], serde_json::Value::Null);
    assert_eq!(panels["panels"].as_array().map(Vec::len), Some(1), "and the stand-in has gone: {panels}");
    assert!(st.panels.channels().is_empty(), "no panel is on a picture, so there is no channel");

    // An idle panel has no picture to edit, and says so.
    let refused = post(at, "/api/v1/set_param", r#"{"id":"size","value":2.0}"#).await;
    assert_eq!(refused.status, 409, "{}", String::from_utf8_lossy(&refused.body));
    // Picking one gives it a channel of its own.
    let picked = ok(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await;
    assert_eq!((picked["device"].as_str(), picked["patch"].as_str()), (Some(id.as_str()), Some("metaballs")));
    assert!(picked["channel"].is_u64());
    // And a route naming a panel that is not there is a 404.
    assert_eq!(post(at, "/api/v1/set_seed", r#"{"seed":1,"panel":"nope"}"#).await.status, 404);
    studio.stop().await;
}
