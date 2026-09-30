//! Card 353's acceptance: **channels own the picture**, end to end, against two
//! `screeny-sim` panels on loopback.
//!
//! The owner, 2026-09-30: *"most of the time I'm going to want multiple panels
//! to be frame-for-frame identical."* So: two panels on one channel are sent
//! **byte-identical pixel payloads** for the same ticks - checked on the
//! datagrams the two simulators received, not on anything the studio says
//! about itself - and the channel encodes once per tick however many panels
//! are on it. A new channel and a panel moved onto it differ; moved back,
//! after its fade, they are identical again. Routes without `channel` are
//! Channel 1's, and `panel` is an alias for that panel's channel.
//!
//! **No test in this file may touch the bench device.** Loopback only, a port
//! band well away from the spec's, mDNS off, and every wait has a deadline.

mod common;

use common::{get, post, studio_and_state, until_json, Ws, PATIENCE};
use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use screeny_sim::{Config as SimConfig, SimDevice};

/// The port band these tests use. Well above 49374/49375, and apart from the
/// bands the other test files use.
const FIRST_PORT: u16 = 51_000;
const LAST_PORT: u16 = 51_100;

/// What one simulated panel was sent - every `FRAME` datagram's sequence
/// number, codec and pixel payload, exactly as it arrived - and what it put
/// up, decoded. Bounded.
/// One frame datagram as a sim received it: sequence number, codec, payload.
type Datagram = (u16, u8, Vec<u8>);

#[derive(Clone, Default)]
struct Seen {
    wire: Arc<Mutex<Vec<Datagram>>>,
    shown: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Seen {
    fn mark(&self) -> (usize, usize) {
        (self.wire.lock().expect("wire").len(), self.shown.lock().expect("shown").len())
    }

    fn wire_since(&self, from: (usize, usize)) -> Vec<(u16, u8, Vec<u8>)> {
        self.wire.lock().expect("wire")[from.0..].to_vec()
    }

    fn shown_since(&self, from: (usize, usize)) -> Vec<Vec<u8>> {
        self.shown.lock().expect("shown")[from.1..].to_vec()
    }
}

/// A simulator with its own id on **consecutive** loopback ports (an
/// `IP:PORT` a human types resolves with the control port taken to be frame +
/// 1), recording every datagram on its frame port and every frame it shows.
fn start_sim(id: &str, avoid: u16) -> (SimDevice, u16, Seen) {
    let seen = Seen::default();
    for port in (FIRST_PORT..LAST_PORT).step_by(2) {
        if port == avoid {
            continue;
        }
        let cfg = SimConfig { frame_port: port, control_port: port + 1, id: id.to_string(), instance: format!("sim-{id}"), ..SimConfig::for_test() };
        let shown = Arc::clone(&seen.shown);
        let sink = Box::new(move |f: &screeny_proto::Rgb888Frame, _: &screeny_sim::FrameMeta| {
            let mut v = shown.lock().expect("shown");
            if v.len() < 20_000 {
                v.push(f.to_vec());
            }
        });
        let wire = Arc::clone(&seen.wire);
        let tap = Box::new(move |datagram: &[u8]| {
            if let Ok(f) = screeny_proto::FramePacket::parse(datagram) {
                if f.flags & screeny_proto::F_FINAL != 0 {
                    return;
                }
                let mut v = wire.lock().expect("wire");
                if v.len() < 20_000 {
                    v.push((f.seq, f.codec, f.payload.to_vec()));
                }
            }
        });
        if let Ok(dev) = SimDevice::start_tapped(cfg, Some(sink), Some(tap)) {
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

/// **Byte for byte**: pair the two panels' datagrams by sequence number - each
/// link numbers its own, from wherever its session started, so the pairing is
/// the offset at which the first of `b`'s payloads appears in `a` - and count
/// the pairs whose codec and payload are identical. Returns (pairs,
/// identical).
fn identical_pairs(a: &[(u16, u8, Vec<u8>)], b: &[(u16, u8, Vec<u8>)]) -> (usize, usize) {
    // The offset every identical payload agrees on (the most common one), so
    // one frame repeated by a still picture cannot mislead it.
    let by_payload: BTreeMap<(u8, &[u8]), u16> = a.iter().map(|(s, c, p)| ((*c, p.as_slice()), *s)).collect();
    let mut offsets: BTreeMap<u16, usize> = BTreeMap::new();
    for (s, c, p) in b {
        if let Some(sa) = by_payload.get(&(*c, p.as_slice())) {
            *offsets.entry(sa.wrapping_sub(*s)).or_default() += 1;
        }
    }
    let Some((&offset, _)) = offsets.iter().max_by_key(|(_, n)| **n) else { return (0, 0) };
    let a_by_seq: BTreeMap<u16, (u8, &[u8])> = a.iter().map(|(s, c, p)| (*s, (*c, p.as_slice()))).collect();
    let mut pairs = 0;
    let mut same = 0;
    for (s, c, p) in b {
        if let Some((ca, pa)) = a_by_seq.get(&s.wrapping_add(offset)) {
            pairs += 1;
            if *ca == *c && *pa == p.as_slice() {
                same += 1;
            }
        }
    }
    (pairs, same)
}

/// How many of `b`'s payloads `a` was also sent, as a fraction.
fn shared_payloads(a: &[(u16, u8, Vec<u8>)], b: &[(u16, u8, Vec<u8>)]) -> f64 {
    let set: HashSet<(u8, &[u8])> = a.iter().map(|(_, c, p)| (*c, p.as_slice())).collect();
    let n = b.iter().filter(|(_, c, p)| set.contains(&(*c, p.as_slice()))).count();
    n as f64 / b.len().max(1) as f64
}

/// For each of `a`'s decoded frames, how far the nearest of `b`'s is - the
/// mean absolute difference per byte - and the median of those. For "these
/// are two different pictures".
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

/// Both panels added, streaming, on Channel 1, on a moving patch.
async fn two_panels(at: SocketAddr, a_port: u16, b_port: u16) {
    for port in [a_port, b_port] {
        ok(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{port}","play":true}}"#)).await;
    }
    until_json(at, PATIENCE * 2, "both panels known by their own ids and connected", "/api/v1/panels", |p| {
        let ids: Vec<&str> = p["panels"].as_array().map(|a| a.iter().filter_map(|x| x["device"].as_str()).collect()).unwrap_or_default();
        ids == ["aa0001", "bb0002"] && p["panels"].as_array().is_some_and(|a| a.iter().all(|x| x["connected"] == true))
    })
    .await;
    ok(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await;
}

/// Wait out a fade (a panel moving, or arriving), and a little more.
async fn settle() {
    tokio::time::sleep(Duration::from_secs_f32(screeny_studio::channel::FADE_MANUAL + 1.0)).await;
}

/// **The card's acceptance.** Two panels on Channel 1 are sent byte-identical
/// payloads, from one encode per tick; a new channel with one of them moved
/// onto it is different; moved back, after its fade, identical again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panels_on_one_channel_are_sent_the_same_bytes() {
    let (_a_dev, a_port, a) = start_sim("aa0001", 0);
    let (_b_dev, b_port, b) = start_sim("bb0002", a_port);
    let (studio, st) = studio_and_state().await;
    let at = studio.addr;
    two_panels(at, a_port, b_port).await;
    settle().await;

    // 1. Both on Channel 1: the same bytes, one encode per tick.
    let panels = get(at, "/api/v1/panels").await.json();
    let (ca, cb) = (card(&panels, "aa0001"), card(&panels, "bb0002"));
    assert_eq!((ca["channel"].as_u64(), cb["channel"].as_u64()), (Some(1), Some(1)), "new panels join Channel 1: {panels}");
    assert_eq!((ca["fading"].as_bool(), cb["fading"].as_bool()), (Some(false), Some(false)), "their fades in are over: {panels}");
    let home = st.panels.home();
    let pa = st.panels.get("aa0001").expect("a");
    let pb = st.panels.get("bb0002").expect("b");
    let (t0, e0, sa0, sb0) = (home.ticks(), home.encodes(), pa.sends(), pb.sends());
    let (ma, mb) = (a.mark(), b.mark());
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (t1, e1, sa1, sb1) = (home.ticks(), home.encodes(), pa.sends(), pb.sends());
    let (wa, wb) = (a.wire_since(ma), b.wire_since(mb));
    let ticks = t1 - t0;
    let encodes = e1 - e0;
    let (pairs, same) = identical_pairs(&wa, &wb);
    eprintln!(
        "one channel: {ticks} ticks, {encodes} encodes; a sent {} shared / {} own, b {} shared / {} own; \
         sims got {} and {} datagrams; {same} of {pairs} seq-paired payloads byte-identical",
        sa1.shared - sa0.shared,
        sa1.own - sa0.own,
        sb1.shared - sb0.shared,
        sb1.own - sb0.own,
        wa.len(),
        wb.len()
    );
    assert!(ticks >= 40, "the channel renders at the full rate while its panels are connected: {ticks}");
    assert!(encodes.abs_diff(ticks) <= 1, "**one encode per tick**, for two panels: {encodes} encodes, {ticks} ticks");
    for (who, s0, s1) in [("a", sa0, sa1), ("b", sb0, sb1)] {
        assert_eq!(s1.own - s0.own, 0, "{who}: not one frame of its own once it is on the channel");
        assert!((s1.shared - s0.shared).abs_diff(ticks) <= 1, "{who}: every tick's shared frame: {} for {ticks} ticks", s1.shared - s0.shared);
    }
    assert!(wa.len() >= 40 && wb.len() >= 40, "both sims were sent frames: {} and {}", wa.len(), wb.len());
    assert!(pairs + 2 >= wb.len().min(wa.len()), "the two streams pair up by sequence: {pairs} pairs of {} and {}", wa.len(), wb.len());
    assert_eq!(same, pairs, "**byte-identical pixel payloads for the same ticks**: {same} of {pairs}");

    // 2. A new channel (a copy of Channel 1), a different picture on it, and
    //    b moved there: b fades, alone, and then the two differ.
    let hall = ok(at, "/api/v1/channels/new", r#"{"name":"Hall"}"#).await;
    assert_eq!((hall["channel"].as_u64(), hall["channel_name"].as_str()), (Some(2), Some("Hall")), "{hall}");
    assert_eq!(hall["patch"], "metaballs", "a copy of Channel 1");
    assert_eq!(hall["panels"], serde_json::json!([]), "with no panels");
    ok(at, "/api/v1/set_patch", r#"{"id":"clocks-dials","channel":2}"#).await;
    let moved = ok(at, "/api/v1/panel/channel", r#"{"panel":"bb0002","channel":2}"#).await;
    assert_eq!((moved["channel"].as_u64(), moved["device"].as_str()), (Some(2), Some("bb0002")), "{moved}");
    assert_eq!(moved["panels"], serde_json::json!(["bb0002"]));
    assert!(pb.fading(), "b fades onto its new channel, frames of its own");
    assert_eq!(get(at, "/api/v1/bootstrap").await.json()["state"]["patch"], "metaballs", "Channel 1 is as it was");
    settle().await;
    let (ma, mb) = (a.mark(), b.mark());
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (wa, wb) = (a.wire_since(ma), b.wire_since(mb));
    let (fa, fb) = (a.shown_since(ma), b.shown_since(mb));
    eprintln!("two channels: {:.0}% of b's payloads also sent to a; decoded nearest {:.2}", shared_payloads(&wa, &wb) * 100.0, nearest(&fa, &fb));
    assert!(wa.len() >= 20 && wb.len() >= 20, "both still streaming: {} and {}", wa.len(), wb.len());
    assert!(shared_payloads(&wa, &wb) < 0.05, "two pictures, two sets of bytes");
    assert!(nearest(&fa, &fb) > 10.0, "and the sims show two different pictures: {:.2}", nearest(&fa, &fb));

    // 3. b moved back: after its fade it is on the shared bytes again.
    let own_before = pb.sends().own;
    ok(at, "/api/v1/panel/channel", r#"{"panel":"bb0002","channel":1}"#).await;
    settle().await;
    let own_after_fade = pb.sends().own;
    assert!(own_after_fade > own_before, "the fade back was frames of its own");
    let (ma, mb) = (a.mark(), b.mark());
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (wa, wb) = (a.wire_since(ma), b.wire_since(mb));
    let (pairs, same) = identical_pairs(&wa, &wb);
    eprintln!("moved back: {same} of {pairs} seq-paired payloads byte-identical");
    assert_eq!(pb.sends().own, own_after_fade, "and none since");
    assert!(pairs >= 20, "{pairs}");
    assert_eq!(same, pairs, "identical again: {same} of {pairs}");

    // Routes without `channel` are Channel 1's - every panel on it - and a
    // `panel` is that panel's channel.
    let seeded = ok(at, "/api/v1/set_seed", r#"{"seed":4242}"#).await;
    assert_eq!((seeded["channel"].as_u64(), seeded["panels"].as_array().map(Vec::len)), (Some(1), Some(2)), "{seeded}");
    ok(at, "/api/v1/panel/channel", r#"{"panel":"bb0002","channel":2}"#).await;
    let via_panel = ok(at, "/api/v1/set_seed", r#"{"seed":7,"panel":"bb0002"}"#).await;
    assert_eq!((via_panel["channel"].as_u64(), via_panel["seed"].as_u64()), (Some(2), Some(7)), "{via_panel}");
    assert_eq!(get(at, "/api/v1/bootstrap").await.json()["state"]["seed"], 4242, "Channel 1 untouched");
    assert_eq!(get(at, "/api/v1/bootstrap?channel=2").await.json()["state"]["seed"], 7);
    assert_eq!(get(at, "/api/v1/bootstrap?panel=bb0002").await.json()["state"]["channel"], 2);

    // Deleting Channel 2 puts b back on Channel 1; Channel 1 cannot go.
    assert_eq!(post(at, "/api/v1/channels/delete", r#"{"channel":1}"#).await.status, 400);
    let after = ok(at, "/api/v1/channels/delete", r#"{"channel":2}"#).await;
    assert_eq!(after["channel"], 1);
    let channels = get(at, "/api/v1/channels").await.json();
    assert_eq!(channels["channels"].as_array().map(Vec::len), Some(1), "{channels}");
    assert_eq!(channels["channels"][0]["panels"], serde_json::json!(["aa0001", "bb0002"]));

    // `set_panel {"on":false}` without a panel lets every panel go.
    let off = ok(at, "/api/v1/set_panel", r#"{"on":false}"#).await;
    assert_eq!(off["on"], false);
    until_json(at, PATIENCE, "both panels let go", "/api/v1/panels", |p| {
        p["panels"].as_array().is_some_and(|a| a.iter().all(|x| x["link"] == "off" && x["connected"] == false))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (ma, mb) = (a.mark(), b.mark());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(a.wire_since(ma).is_empty() && b.wire_since(mb).is_empty(), "no more frames once let go");
    studio.stop().await;
}

/// Channels as the API has them: new, rename, delete, the retired routes, and
/// what is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channels_are_made_renamed_and_deleted() {
    let (studio, _st) = studio_and_state().await;
    let at = studio.addr;
    let list = get(at, "/api/v1/channels").await.json();
    assert_eq!(list["channels"].as_array().map(Vec::len), Some(1), "a fresh studio has Channel 1: {list}");
    assert_eq!((list["channels"][0]["id"].as_u64(), list["channels"][0]["name"].as_str(), list["channels"][0]["home"].as_bool()), (Some(1), Some("Channel 1"), Some(true)));

    ok(at, "/api/v1/set_seed", r#"{"seed":31}"#).await;
    let two = ok(at, "/api/v1/channels/new", "{}").await;
    assert_eq!((two["channel"].as_u64(), two["channel_name"].as_str(), two["seed"].as_u64()), (Some(2), Some("Channel 2"), Some(31)), "a copy of Channel 1: {two}");
    let three = ok(at, "/api/v1/channels/new", r#"{"name":"Kitchen","from":2}"#).await;
    assert_eq!((three["channel"].as_u64(), three["channel_name"].as_str()), (Some(3), Some("Kitchen")));
    assert_eq!(post(at, "/api/v1/channels/new", r#"{"from":9}"#).await.status, 404);
    assert_eq!(post(at, "/api/v1/channels/rename", r#"{"channel":3,"name":"   "}"#).await.status, 400);
    let renamed = ok(at, "/api/v1/channels/rename", r#"{"channel":3,"name":"Hall"}"#).await;
    assert_eq!(renamed["channel_name"], "Hall");
    assert_eq!(post(at, "/api/v1/channels/rename", r#"{"channel":9,"name":"x"}"#).await.status, 404);

    // Picture routes by channel, and one that is not there.
    let dials = ok(at, "/api/v1/set_patch", r#"{"id":"clocks-dials","channel":3}"#).await;
    assert_eq!((dials["channel"].as_u64(), dials["patch"].as_str()), (Some(3), Some("clocks-dials")));
    assert_eq!(get(at, "/api/v1/bootstrap?channel=1").await.json()["state"]["patch"], "clocks-numerals", "Channel 1 as it was");
    assert_eq!(post(at, "/api/v1/set_seed", r#"{"seed":1,"channel":9}"#).await.status, 404);
    assert_eq!(post(at, "/api/v1/set_seed", r#"{"seed":1,"panel":"nope"}"#).await.status, 404);
    // Card 356: output is the studio's - one setting, for every channel. A
    // `channel` (an older client's) is accepted and does not scope it.
    let mut output = dials["output"].clone();
    assert_eq!((output["panel"].as_str(), output["dither"].as_str()), (Some("aligned_dark"), Some("blue_noise")), "the live default");
    output["dither"] = "bayer4".into();
    let out = ok(at, "/api/v1/set_output", &serde_json::json!({ "output": output, "channel": 3 }).to_string()).await;
    assert_eq!(out["output"]["dither"], "bayer4");
    assert_eq!(get(at, "/api/v1/bootstrap").await.json()["state"]["output"]["dither"], "bayer4", "Channel 1 has it too");
    for c in get(at, "/api/v1/channels").await.json()["channels"].as_array().expect("list") {
        assert_eq!(c["output"]["dither"], "bayer4", "every channel reports the studio's: {c}");
    }
    let stale = ok(at, "/api/v1/set_output", &serde_json::json!({ "output": output, "channel": 99 }).to_string()).await;
    assert_eq!(stale["output"]["dither"], "bayer4", "a channel that is gone is not an error");

    // Card 350's implicit ways of sharing are retired, and say what to use.
    for route in ["/api/v1/same_as", "/api/v1/detach"] {
        let r = post(at, route, r#"{"as":"x"}"#).await;
        assert_eq!(r.status, 410, "{route}");
        assert!(r.json()["error"].as_str().is_some_and(|e| e.contains("/channels/new")), "{route}");
    }

    let gone = ok(at, "/api/v1/channels/delete", r#"{"channel":2}"#).await;
    assert_eq!(gone["channel"], 1);
    let ids: Vec<u64> = get(at, "/api/v1/channels").await.json()["channels"].as_array().expect("list").iter().filter_map(|c| c["id"].as_u64()).collect();
    assert_eq!(ids, [1, 3]);
    assert_eq!(ok(at, "/api/v1/channels/new", "{}").await["channel"], 4, "ids are never reused");
    studio.stop().await;
}

/// Sockets are about a channel: `?channel=`, or `?panel=` as that panel's
/// channel - following it when it moves - and they are sent the channels.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_socket_is_about_a_channel() {
    let (_a_dev, a_port, _a) = start_sim("aa0001", 0);
    let (_b_dev, b_port, _b) = start_sim("bb0002", a_port);
    let (studio, _st) = studio_and_state().await;
    let at = studio.addr;
    two_panels(at, a_port, b_port).await;
    ok(at, "/api/v1/channels/new", r#"{"name":"Hall"}"#).await;
    ok(at, "/api/v1/set_patch", r#"{"id":"clocks-dials","channel":2}"#).await;

    let mut thumb = Ws::connect_asking(at, "channel=2&fps=4&overview=false").await;
    let hello = thumb.event("state").await;
    assert_eq!((hello["channel"].as_u64(), hello["state"]["patch"].as_str()), (Some(2), Some("clocks-dials")), "{hello}");
    thumb.frame().await;
    let t = thumb.measure(Duration::from_secs(2)).await;
    assert!((5..=10).contains(&t.frames), "about four frames a second of an empty, watched channel: {} in 2 s", t.frames);

    // A socket about a panel is about its channel, and follows it.
    let mut b_sock = Ws::connect_asking(at, "panel=bb0002&fps=0&overview=false").await;
    let hello = b_sock.event("state").await;
    assert_eq!((hello["channel"].as_u64(), hello["panel"].as_str()), (Some(1), Some("bb0002")), "{hello}");
    ok(at, "/api/v1/panel/channel", r#"{"panel":"bb0002","channel":2}"#).await;
    let on_two = |t: &str| serde_json::from_str::<serde_json::Value>(t).is_ok_and(|v| v["type"] == "state" && v["channel"] == 2);
    let moved = b_sock.next_matching(|m| matches!(m, common::Msg::Text(t) if on_two(t))).await;
    if let common::Msg::Text(t) = moved {
        let v: serde_json::Value = serde_json::from_str(&t).expect("json");
        assert_eq!((v["state"]["patch"].as_str(), v["panel"].as_str()), (Some("clocks-dials"), Some("bb0002")), "{v}");
    }

    // The page's socket, without either, is Channel 1's and is sent every
    // panel and every channel.
    let mut page = Ws::connect_asking(at, "fps=0").await;
    let hello = page.event("state").await;
    assert_eq!(hello["channel"], 1);
    let channels = page.event("channels").await;
    let names: Vec<&str> = channels["channels"].as_array().expect("channels").iter().filter_map(|c| c["name"].as_str()).collect();
    assert_eq!(names, ["Channel 1", "Hall"], "{channels}");

    // A socket about a channel or a panel that is not there is told so.
    let mut nobody = Ws::connect_asking(at, "channel=9").await;
    let err = nobody.event("error").await;
    assert!(err["error"].as_str().is_some_and(|e| e.contains("channel 9")), "{err}");
    let mut nobody = Ws::connect_asking(at, "panel=nope").await;
    let err = nobody.event("error").await;
    assert!(err["error"].as_str().is_some_and(|e| e.contains("nope")), "{err}");
    studio.stop().await;
}

/// **A new panel joins Channel 1**: a device the registry learns about - here
/// added straight to it, as a browse would - becomes a panel within a
/// supervisor tick, on Channel 1 and lit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_the_registry_learns_about_joins_channel_1() {
    let (studio, st) = studio_and_state().await;
    let at = studio.addr;
    let before = get(at, "/api/v1/panels").await.json();
    assert_eq!(before["panels"], serde_json::json!([]), "a fresh studio has no panel: {before}");

    // Nothing listens on port 9 of loopback: the device is known, never heard.
    let id = st.devices.add_manual("127.0.0.1:9", "").expect("added");
    let panels = until_json(at, PATIENCE, "the device adopted as a panel", "/api/v1/panels", |p| {
        p["panels"].as_array().is_some_and(|a| a.iter().any(|x| x["device"] == id))
    })
    .await;
    let p = card(&panels, &id);
    assert_eq!((p["channel"].as_u64(), p["channel_name"].as_str(), p["on"].as_bool()), (Some(1), Some("Channel 1"), Some(true)), "{panels}");
    assert_ne!(p["link"], "off");
    let state = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(state["state"]["panels"], serde_json::json!([id]), "{state}");
    studio.stop().await;
}
