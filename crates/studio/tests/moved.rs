//! Card 141: a panel that has moved is found again, and nothing else is
//! disturbed.
//!
//! **No test here may touch the bench device.** mDNS is off in every studio
//! these tests start (`discover: false`), so nothing multicasts; the probe is
//! pointed at an explicit list of **loopback** control addresses, well above
//! the spec's ports, so not one datagram can leave this machine. That list is
//! this file's stand-in for a subnet broadcast: on a LAN a probe reaches every
//! address at once, and here it reaches every address in a small band of
//! loopback ports. The studio is never told which of them the panel moved to -
//! it learns that from the `id=` in the answer, which is the whole of the
//! card.
//!
//! Every wait has a deadline and every simulator is dropped by the test that
//! started it.

mod common;

use common::{get, post, until_json, Temp};
use std::net::SocketAddr;
use std::time::Duration;

use screeny_sim::{Config as SimConfig, SimDevice};
use screeny_studio::{Config, Running, Studio};

/// Long enough for a loaded bench, short enough that a wedged test fails
/// rather than hangs.
const PATIENCE: Duration = Duration::from_secs(30);

/// The band of loopback ports the first test's panels live in: frame ports
/// `BAND`, `BAND+2`, ..., and the control port beside each. The second test
/// has a band of its own, because the two run at the same time in one binary
/// and each probes its whole band.
///
/// Clear of `tests/fleet.rs` (50800..50900) and `tests/device_status.rs`
/// (50800..50920, 51000..51060), so two test binaries running at once do not
/// take each other's ports either.
const BAND: u16 = 50_940;
/// The second test's band.
const BAND2: u16 = BAND + PAIRS * 2;
/// How many frame/control pairs are in each. Three panels' worth of room, so
/// a panel that moves has somewhere to move to.
const PAIRS: u16 = 6;

/// Every control address in a band: what the probe is pointed at, in place of
/// a subnet broadcast address.
fn probe_to(base: u16) -> Vec<SocketAddr> {
    (0..PAIRS).map(|i| SocketAddr::from(([127, 0, 0, 1], base + i * 2 + 1))).collect()
}

/// A simulator on a **free** pair in the band, and the frame port it got.
///
/// Consecutive frame/control ports, because an `IP:PORT` a human types - and
/// a `GET_INFO` answer - resolve with the control port taken to be frame + 1.
/// `avoid` is a port already in use by this test, so "it came back somewhere
/// else" is really somewhere else.
fn start_sim(base: u16, id: &str, http: Option<u16>, avoid: &[u16]) -> (SimDevice, u16) {
    for i in 0..PAIRS {
        let frame = base + i * 2;
        if avoid.contains(&frame) {
            continue;
        }
        let cfg = SimConfig {
            frame_port: frame,
            control_port: frame + 1,
            id: id.to_string(),
            instance: format!("sim-{id}"),
            name: String::new(),
            http: http.is_some(),
            http_port: http.unwrap_or(0),
            http_port_explicit: http.is_some(),
            ..SimConfig::for_test()
        };
        if let Ok(dev) = SimDevice::start(cfg) {
            return (dev, frame);
        }
    }
    panic!("no free port pair in {base}..{}", base + PAIRS * 2);
}

/// How long a panel may be unheard before this studio decides it is somewhere
/// else. Seconds rather than the product's two minutes, because a test cannot
/// wait two minutes to cross that line - but not *one* second: the telemetry
/// poll walks the devices in turn and a dead one costs the pass its control
/// timeout, so with a shorter line a panel that is streaming perfectly well
/// can be marked stale for a pass simply because its turn came late.
const STALE_AFTER: Duration = Duration::from_secs(3);

/// A studio that probes the band and gives up on an address quickly.
///
/// `discover_every` is the probe's own tick.
async fn studio_probing(base: u16, dir: &std::path::Path, device_http: Option<u16>) -> Running {
    let cfg = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        state_dir: Some(dir.to_path_buf()),
        discover: false,
        probe_to: probe_to(base),
        discover_every: Duration::from_millis(300),
        supervise_every: Duration::from_millis(200),
        telemetry_every: Duration::from_millis(200),
        stale_after: STALE_AFTER,
        device_http: device_http.is_some(),
        device_http_every: Duration::from_millis(300),
        device_http_port: device_http.unwrap_or(80),
        ..Config::default()
    };
    Studio::bind(cfg).await.expect("bind an ephemeral loopback port").spawn()
}

/// The device with this id in `/api/v1/status`.
fn device(status: &serde_json::Value, id: &str) -> serde_json::Value {
    status["devices"]
        .as_array()
        .and_then(|ds| ds.iter().find(|d| d["id"] == id))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// Add a panel by address and start playing on it, the way the page does.
async fn add_and_play(at: SocketAddr, port: u16, name: &str) {
    let added = post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{port}","name":"{name}","play":true}}"#)).await;
    assert_eq!(added.status, 200, "{}", String::from_utf8_lossy(&added.body));
}

/// Wait until `id` is resolved and streaming, and hand back **that** answer.
async fn wait_streaming(at: SocketAddr, id: &str, what: &str) -> serde_json::Value {
    until_json(at, PATIENCE, what, "/api/v1/status", |s| {
        let d = device(s, id);
        d["resolved"] == true
            && d["player"]["panel"]["connected"] == true
            && d["player"]["panel"]["frames_sent"].as_u64().unwrap_or(0) > 5
    })
    .await
}

/// **The card's deliverable.** Two panels; one of them comes back on a
/// different port with the same `id=`; the studio catches up with nobody
/// saying anything, the panel keeps its player, its patch and its seed, and
/// the panel that did not move is not touched at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panel_that_moved_is_followed_and_the_other_is_left_alone() {
    let dir = Temp::new("moved");
    let (mover, was_at) = start_sim(BAND, "mov001", None, &[]);
    let (_stayer, stays_at) = start_sim(BAND, "sta002", None, &[was_at]);
    let studio = studio_probing(BAND, &dir.0, None).await;
    let at = studio.addr;

    add_and_play(at, was_at, "wanderer").await;
    add_and_play(at, stays_at, "homebody").await;
    wait_streaming(at, "mov001", "the panel that will move to start playing").await;
    wait_streaming(at, "sta002", "the panel that stays to start playing").await;

    // Something specific on each, so "it kept what it was playing" is a thing
    // a test can check rather than a hope.
    post(at, "/api/v1/player/set", r#"{"device":"mov001","patch":"metaballs","seed":4242,"fps":30}"#).await;
    post(at, "/api/v1/player/set", r#"{"device":"sta002","patch":"plasma","seed":1717,"fps":30}"#).await;
    let before = until_json(at, PATIENCE, "both panels to take their patch", "/api/v1/status", |s| {
        device(s, "mov001")["player"]["patch"] == "metaballs" && device(s, "sta002")["player"]["patch"] == "plasma"
    })
    .await;
    let stayer_before = device(&before, "sta002");
    let frames_before = stayer_before["player"]["panel"]["frames_sent"].as_u64().unwrap_or(0);
    // Which panel the page is a window onto, so that "nothing else moved" can
    // include that. It is the panel added last, because adding one with
    // `play` attaches it.
    let attached_before = before["preview"]["device"].as_str().unwrap_or_default().to_string();

    // **The move.** The panel goes away and comes back somewhere else, with
    // its own id and nothing else about it changed. Nobody tells the studio.
    drop(mover);
    let (_moved, now_at) = start_sim(BAND, "mov001", None, &[was_at, stays_at]);
    assert_ne!(now_at, was_at, "the panel has to come back somewhere else for this to be a move");

    // One answer, about one moment: what is asserted below and what was
    // waited for have to be the same read, including the panel that stayed.
    let after = until_json(at, PATIENCE, "the studio to find the panel at its new address", "/api/v1/status", |s| {
        let d = device(s, "mov001");
        let stayer = device(s, "sta002");
        d["control_addr"] == serde_json::json!(format!("127.0.0.1:{}", now_at + 1))
            && d["resolved"] == true
            && d["player"]["panel"]["connected"] == true
            && stayer["resolved"] == true
            && stayer["player"]["panel"]["connected"] == true
    })
    .await;

    // It is the same panel: same id, same player, same patch, same seed.
    let d = device(&after, "mov001");
    assert_eq!(d["id"], "mov001");
    assert_eq!(d["address"], format!("127.0.0.1:{now_at}"), "the address followed it: {d}");
    assert_eq!(d["label"], "wanderer", "and it is still the panel that was added: {d}");
    assert_eq!(d["player"]["patch"], "metaballs", "it lost what it was playing: {d}");
    assert_eq!(d["player"]["seed"], 4242, "it lost its seed: {d}");
    assert_eq!(d["player"]["fps"], 30.0);
    assert_eq!(after["devices"].as_array().map(Vec::len), Some(2), "a move must never make a third device");
    assert_eq!(
        after["preview"]["device"], attached_before,
        "following a panel must not change which one the page is a window onto"
    );

    // The probe happened, and said so in the one place a person would look.
    assert!(after["discovery"]["probes"].as_u64().unwrap_or(0) >= 1, "{}", after["discovery"]);
    assert_eq!(after["discovery"]["moved"], 1, "exactly one panel moved: {}", after["discovery"]);
    assert_eq!(after["discovery"]["last_probe_error"], serde_json::Value::Null);

    // The panel that did not move was not touched: same address, same ports,
    // never disconnected, and still sending frames from the same player.
    let stayer = device(&after, "sta002");
    assert_eq!(stayer["address"], format!("127.0.0.1:{stays_at}"), "{stayer}");
    assert_eq!(stayer["control_addr"], format!("127.0.0.1:{}", stays_at + 1), "{stayer}");
    assert_eq!(stayer["resolved"], true, "its resolution was thrown away: {stayer}");
    assert_eq!(stayer["player"]["patch"], "plasma", "{stayer}");
    assert_eq!(stayer["player"]["seed"], 1717, "{stayer}");
    assert_eq!(stayer["player"]["panel"]["connected"], true, "its link was dropped: {stayer}");
    let frames_after = stayer["player"]["panel"]["frames_sent"].as_u64().unwrap_or(0);
    assert!(frames_after > frames_before, "it stopped streaming: {frames_before} -> {frames_after}");
    assert_eq!(
        stayer["player"]["panel"]["sessions"], stayer_before["player"]["panel"]["sessions"],
        "the panel that did not move should not have reconnected: {stayer}"
    );

    assert_eq!(get(at, "/healthz").await.status, 200, "a panel moving is not an unhealthy studio");
    println!(
        "card 141: `mov001` moved {was_at} -> {now_at}, followed after {} probe(s); `sta002` untouched at {stays_at}, {frames_before} -> {frames_after} frames",
        after["discovery"]["probes"]
    );
    studio.stop().await;
}

/// The other half of following a panel: the **HTTP** status poll (card 180)
/// goes with it, because `DeviceRecord::http_addr` is derived from the
/// resolved address rather than stored beside it. Nobody says anything about
/// HTTP here; it simply starts working again at the new resolution.
///
/// On loopback the IP cannot change, so what this shows is that the poll
/// resumes across the move. That the address it resumes at is the *new* IP is
/// `devices::tests::a_probe_follows_a_device_that_answers_from_a_new_address`,
/// which can move a panel between two addresses that do not exist.
///
/// The panel is away for longer than `stale_after` before it comes back, as a
/// panel taking a new lease is. That matters here and nowhere else: on
/// loopback the replacement takes the **same** HTTP port back, and a status
/// read that keeps working is the studio hearing from the device - so without
/// a real gap the panel would never count as unheard and nothing would ever
/// be probed for. On a real move the address changes and both stop together.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn the_status_poll_follows_a_panel_that_moved() {
    let dir = Temp::new("movedhttp");
    // A known HTTP port: the studio reads one port for every device, and the
    // replacement simulator takes the same one back.
    let http_port = 51_090;
    let (mover, was_at) = start_sim(BAND2, "mov003", Some(http_port), &[]);
    let studio = studio_probing(BAND2, &dir.0, Some(http_port)).await;
    let at = studio.addr;

    add_and_play(at, was_at, "wanderer").await;
    let before = until_json(at, PATIENCE, "the panel's own status API to be read", "/api/v1/status", |s| {
        device(s, "mov003")["http"]["reads"].as_u64().unwrap_or(0) > 0
    })
    .await;
    let reads_before = device(&before, "mov003")["http"]["reads"].as_u64().unwrap_or(0);

    drop(mover);
    tokio::time::sleep(STALE_AFTER + Duration::from_millis(500)).await;
    let (_moved, now_at) = start_sim(BAND2, "mov003", Some(http_port), &[was_at]);
    assert_ne!(now_at, was_at);

    let after = until_json(at, PATIENCE, "the status poll to start working again", "/api/v1/status", |s| {
        let d = device(s, "mov003");
        d["control_addr"] == serde_json::json!(format!("127.0.0.1:{}", now_at + 1))
            && d["http"]["reads"].as_u64().unwrap_or(0) > reads_before
            && d["facts"]["heap_size"].is_number()
    })
    .await;
    let d = device(&after, "mov003");
    assert_eq!(d["http"]["last_error"], serde_json::Value::Null, "{}", d["http"]);
    assert_eq!(d["http"]["absent"], false);
    assert_eq!(get(at, "/healthz").await.status, 200);
    println!(
        "card 141: `mov003` moved {was_at} -> {now_at}; status reads {reads_before} -> {}, facts {} s old",
        d["http"]["reads"], d["facts_ago"]
    );
    studio.stop().await;
}
