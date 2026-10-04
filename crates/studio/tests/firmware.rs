//! Card 364: firmware updates from the Studio (and so from Home Assistant),
//! the whole flow, against the simulator.
//!
//! The simulator models an update when asked (`SimHandle::model_ota`, card
//! 246): the old image keeps answering for a moment after the upload's reply,
//! then nothing answers, then a new `boot_id` comes up **on trial** with the
//! uploaded version, then it confirms - or, with `model_ota_reverts`, rolls
//! back to the old image. Its upload route runs the device's own image
//! validator, so an image the panel would refuse is refused here.
//!
//! **No test in this file may touch the bench device.** Loopback only, ports
//! well above the spec's, mDNS off, and every wait has a deadline.

mod common;

use common::{get, post, until_json};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use screeny_fwimage::build::Builder;
use screeny_sim::{Config as SimConfig, OtaTiming, SimDevice};
use screeny_studio::firmware::{Offer, Timing};
use screeny_studio::ha::bridge;
use screeny_studio::ha::{Command, Order, Target};
use screeny_studio::{AppState, Config, Running, Studio};

const PATIENCE: Duration = Duration::from_secs(30);

/// Tenths of a second, in the order and proportion of a device's two seconds /
/// fifteen / sixty.
const FAST: OtaTiming = OtaTiming {
    old_image_for: Duration::from_millis(300),
    away_for: Duration::from_millis(300),
    trial_for: Duration::from_millis(600),
};

fn timing() -> Timing {
    Timing {
        upload: Duration::from_secs(20),
        reappear: Duration::from_secs(4),
        decide: Duration::from_secs(8),
        poll: Duration::from_millis(50),
    }
}

fn image(version: &'static str) -> Vec<u8> {
    Builder { version, ..Builder::good() }.build()
}

/// A simulator on loopback with known ports, running firmware `fw`.
fn start_sim(fw: &str) -> (SimDevice, u16, u16) {
    for i in 0..60u16 {
        let (frame, http) = (52_800 + i * 2, 53_000 + i);
        let cfg = SimConfig {
            frame_port: frame,
            control_port: frame + 1,
            http: true,
            http_port: http,
            http_port_explicit: true,
            fw: fw.to_string(),
            ..SimConfig::for_test()
        };
        if let Ok(dev) = SimDevice::start_with(cfg, None) {
            return (dev, frame, http);
        }
    }
    panic!("no free simulator ports");
}

/// A TCP relay in front of the simulator's HTTP port that can be told to
/// **die in the middle of a firmware upload**: it forwards everything, and for
/// a `POST` it forwards `cut_after` bytes of the body and then drops both
/// sockets - a panel that went away mid-upload, as the Studio sees it.
struct Relay {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
}

impl Relay {
    fn start(upstream: SocketAddr, cut_after: usize) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a relay port");
        listener.set_nonblocking(true).expect("non-blocking");
        let addr = listener.local_addr().expect("its address");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let _ = client.set_nonblocking(false);
                        std::thread::spawn(move || relay(client, upstream, cut_after));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Relay { addr, stop }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn relay(mut client: TcpStream, upstream: SocketAddr, cut_after: usize) {
    let Ok(mut up) = TcpStream::connect(upstream) else { return };
    let _ = client.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = up.set_read_timeout(Some(Duration::from_secs(10)));
    // The request head.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match client.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let post = head.starts_with(b"POST");
    let _ = up.write_all(&head);
    if post {
        let mut left = cut_after;
        let mut buf = [0u8; 4096];
        while left > 0 {
            let n = match client.read(&mut buf[..left.min(4096)]) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if up.write_all(&buf[..n]).is_err() {
                break;
            }
            left -= n;
        }
        // The panel is gone: nothing more, in either direction.
        let _ = up.shutdown(std::net::Shutdown::Both);
        let _ = client.shutdown(std::net::Shutdown::Both);
        return;
    }
    // Anything else: a transparent relay until the upstream closes.
    let mut up_back = up.try_clone().expect("clone");
    let mut client_back = client.try_clone().expect("clone");
    let t = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = up_back.read(&mut buf) {
            if n == 0 || client_back.write_all(&buf[..n]).is_err() {
                break;
            }
        }
        let _ = client_back.shutdown(std::net::Shutdown::Both);
    });
    let mut buf = [0u8; 4096];
    while let Ok(n) = client.read(&mut buf) {
        if n == 0 || up.write_all(&buf[..n]).is_err() {
            break;
        }
    }
    let _ = t.join();
}

/// What a test starts with.
struct Rig {
    dev: SimDevice,
    studio: Running,
    st: AppState,
    id: String,
    _tmp: Option<common::Temp>,
    _relay: Option<Relay>,
}

#[derive(Default)]
struct Setup {
    /// What the panel runs.
    panel: &'static str,
    /// What the Studio carries; `None` carries nothing.
    offer: Option<&'static str>,
    /// Offered as it is, unchecked: a Studio that offers what a panel refuses.
    unchecked: Option<Vec<u8>>,
    /// Model the update at all; `false` is a panel that accepts an image and
    /// does not restart (the simulator's default).
    model: bool,
    revert: bool,
    /// The panel's HTTP goes through a relay that dies this many bytes into an
    /// upload.
    cut_after: Option<usize>,
    /// The panel stays away for ever once it has gone.
    gone_for_good: bool,
    /// No HTTP server on the panel at all.
    no_http: bool,
}

async fn rig(s: Setup) -> Rig {
    let (dev, frame, http) = start_sim(s.panel);
    if s.model {
        dev.handle().model_ota(Some(OtaTiming {
            away_for: if s.gone_for_good { Duration::from_secs(3600) } else { FAST.away_for },
            ..FAST
        }));
        dev.handle().model_ota_reverts(s.revert);
    }
    let sim_http = dev.http_addr().expect("the HTTP API is on");
    assert_eq!(sim_http.port(), http);
    let relay = s.cut_after.map(|n| Relay::start(sim_http, n));
    let http_port = relay.as_ref().map_or(http, |r| r.addr.port());

    let tmp = common::Temp::new(&format!("fw{frame}"));
    let mut cfg = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        supervise_every: Duration::from_millis(200),
        telemetry_every: Duration::from_millis(200),
        device_http_every: Duration::from_millis(300),
        device_http: !s.no_http,
        device_http_port: http_port,
        firmware_timing: timing(),
        ..Config::default()
    };
    if let Some(version) = s.offer {
        let path = tmp.0.join("screeny-fw.bin");
        std::fs::write(&path, image(version)).expect("write the image");
        cfg.firmware = Some(path);
    }
    if let Some(bytes) = s.unchecked {
        cfg.firmware_offer = Some(Offer { version: s.offer.unwrap_or("9.9.9").to_string(), bytes: bytes.into() });
    }
    let studio = Studio::bind(cfg).await.expect("bind an ephemeral loopback port");
    let st = studio.state();
    let studio = studio.spawn();
    let body = format!(r#"{{"to":"127.0.0.1:{frame}","name":"bench","play":false}}"#);
    let added = post(studio.addr, "/api/v1/devices/add", &body).await;
    assert_eq!(added.status, 200, "{}", String::from_utf8_lossy(&added.body));
    until_json(studio.addr, PATIENCE, "the studio to resolve the simulator", "/api/v1/status", |v| v["devices"][0]["resolved"] == true).await;
    if !s.no_http {
        until_json(studio.addr, PATIENCE, "the panel's own status", "/api/v1/status", |v| !v["devices"][0]["facts"].is_null()).await;
    }
    let id = added.json()["id"].as_str().expect("an id").to_string();
    let id = st.devices.ids().into_iter().next().unwrap_or(id);
    Rig { dev, studio, st, id, _tmp: Some(tmp), _relay: relay }
}

impl Rig {
    async fn update(&self) -> serde_json::Value {
        get(self.studio.addr, "/api/v1/status").await.json()["devices"][0]["update"].clone()
    }

    async fn install(&self) -> common::Response {
        post(self.studio.addr, "/api/v1/device/firmware", &format!(r#"{{"device":"{}","confirm":true}}"#, self.id)).await
    }

    /// Wait until the attempt has ended one way or the other; returns the view.
    async fn until_settled(&self, what: &str) -> serde_json::Value {
        let v = until_json(self.studio.addr, PATIENCE, what, "/api/v1/status", |v| {
            let u = &v["devices"][0]["update"];
            let phase = u["run"]["phase"].as_str();
            // Started (we have seen a run) and not going any more.
            !matches!(phase, Some("uploading" | "restarting" | "trial")) && (phase == Some("failed") || u["run"].is_null())
        })
        .await;
        v["devices"][0]["update"].clone()
    }
}

/// **The acceptance**: offer, push, version change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_update_is_offered_pushed_and_the_panel_comes_back_on_the_new_version() {
    let r = rig(Setup { panel: "0.10.0", offer: Some("0.11.0"), model: true, ..Setup::default() }).await;

    // The offer: what the panel runs, what is carried, and that Install works.
    let u = r.update().await;
    assert_eq!((u["installed"].as_str(), u["offered"].as_str(), u["available"].clone()), (Some("0.10.0"), Some("0.11.0"), true.into()), "{u}");
    // ... and what Home Assistant's update entity would be told.
    let fw = bridge::fleet_of(&r.st).panels[0].snapshot.firmware.clone();
    assert_eq!((fw.installed.as_deref(), fw.latest.as_deref(), fw.in_progress), (Some("0.10.0"), Some("0.11.0"), false));

    let before = r.dev.handle().boot_id();
    let started = r.install().await;
    assert_eq!(started.status, 200, "{}", String::from_utf8_lossy(&started.body));
    assert_eq!(started.json()["to"], "0.11.0");

    // While it is going on the panel is not available for a second Install,
    // and Home Assistant is told it is in progress.
    let going = r.install().await;
    assert_eq!(going.status, 409, "a second Install while one runs is refused: {}", String::from_utf8_lossy(&going.body));
    assert!(bridge::fleet_of(&r.st).panels[0].snapshot.firmware.in_progress);

    let done = r.until_settled("the update to finish").await;
    assert!(done["run"].is_null(), "it worked, so there is no failure to show: {done}");
    // The panel really is on the new image: a different boot, the version that
    // was uploaded, confirmed.
    let now = get(r.studio.addr, "/api/v1/status").await.json();
    let facts = &now["devices"][0]["facts"];
    assert_eq!(facts["fw"], "0.11.0", "{facts}");
    assert_eq!(facts["fw_state"], "valid", "{facts}");
    assert_ne!(facts["boot_id"].as_u64(), Some(u64::from(before)), "the panel restarted");
    assert_eq!(r.dev.handle().ota_phase(), screeny_sim::OtaPhase::Confirmed);
    // The reboot was asked for, so it is not an "unasked" one.
    assert_eq!(facts["unasked_reboots"], 0, "{facts}");
    // And there is nothing left to install.
    assert_eq!(now["devices"][0]["update"]["available"], false, "{now}");
    let fw = bridge::fleet_of(&r.st).panels[0].snapshot.firmware.clone();
    assert_eq!((fw.installed.as_deref(), fw.latest.as_deref(), fw.in_progress), (Some("0.11.0"), Some("0.11.0"), false));
    drop(r.studio);
}

/// Home Assistant's Install goes through the same function as the page's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_assistants_install_starts_the_same_update() {
    let r = rig(Setup { panel: "0.10.0", offer: Some("0.11.0"), model: true, ..Setup::default() }).await;
    bridge::execute(&r.st, &Order { target: Target::Panel(r.id.clone()), command: Command::InstallFirmware }).expect("Install is accepted");
    assert!(r.st.firmware.active(&r.id), "an update is running");
    let done = r.until_settled("the update to finish").await;
    assert!(done["run"].is_null(), "{done}");
    assert_eq!(r.dev.handle().ota_phase(), screeny_sim::OtaPhase::Confirmed);
    // Nothing to install now: Install is refused, in a sentence.
    let again = bridge::execute(&r.st, &Order { target: Target::Panel(r.id.clone()), command: Command::InstallFirmware });
    assert!(again.is_err(), "{again:?}");
}

/// Never without an explicit action, and the page's second lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_updates_by_itself_and_the_page_must_confirm() {
    let r = rig(Setup { panel: "0.10.0", offer: Some("0.11.0"), model: true, ..Setup::default() }).await;
    let no_confirm = post(r.studio.addr, "/api/v1/device/firmware", &format!(r#"{{"device":"{}"}}"#, r.id)).await;
    assert_eq!(no_confirm.status, 400);
    let nobody = post(r.studio.addr, "/api/v1/device/firmware", r#"{"device":"nobody","confirm":true}"#).await;
    assert_eq!(nobody.status, 404);
    // Wait several polls: an offer that is not accepted is never acted on.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(r.dev.handle().ota_phase(), screeny_sim::OtaPhase::Idle, "nothing was uploaded");
    assert_eq!(r.update().await["available"], true);
}

/// Decision 4: a panel on the same or a newer firmware is offered nothing, and
/// Install is refused - never a downgrade.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panel_that_is_up_to_date_or_newer_is_offered_nothing() {
    for panel in ["0.11.0", "0.12.0"] {
        let r = rig(Setup { panel, offer: Some("0.11.0"), model: true, ..Setup::default() }).await;
        let u = r.update().await;
        assert_eq!(u["available"], false, "{panel}: {u}");
        let refused = r.install().await;
        assert_eq!(refused.status, 409, "{panel}: {}", String::from_utf8_lossy(&refused.body));
        assert_eq!(r.dev.handle().ota_phase(), screeny_sim::OtaPhase::Idle, "{panel}: nothing was uploaded");
        // HA is shown the installed version as the latest: up to date, no downgrade.
        let fw = bridge::fleet_of(&r.st).panels[0].snapshot.firmware.clone();
        assert_eq!((fw.installed.as_deref(), fw.latest.as_deref()), (Some(panel), Some(panel)));
    }
}

/// No image in the Studio: no update is offered and nothing else changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_studio_with_no_firmware_offers_nothing() {
    let r = rig(Setup { panel: "0.10.0", offer: None, model: true, ..Setup::default() }).await;
    let u = r.update().await;
    assert_eq!((u["available"].clone(), u["offered"].clone()), (false.into(), serde_json::Value::Null), "{u}");
    assert_eq!(u["installed"], "0.10.0", "the panel's firmware is still shown");
    assert_eq!(r.install().await.status, 409);
}

/// Failure: the panel refuses the image. Shown, and retryable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_image_is_a_reported_failure() {
    let wrong = Builder::wrong_project().build();
    let r = rig(Setup { panel: "0.10.0", offer: Some("0.11.0"), unchecked: Some(wrong), model: true, ..Setup::default() }).await;
    assert_eq!(r.install().await.status, 200);
    let done = r.until_settled("the refusal").await;
    let why = done["run"]["why"].as_str().unwrap_or_default().to_string();
    assert_eq!(done["run"]["phase"], "failed", "{done}");
    assert!(why.contains("refused") && why.contains("not screeny firmware"), "{why}");
    assert_eq!(r.dev.handle().ota_phase(), screeny_sim::OtaPhase::Idle, "the panel did not switch");
    // HA is told the panel is not updating, and why it did not.
    let fw = bridge::fleet_of(&r.st).panels[0].snapshot.firmware.clone();
    assert!(!fw.in_progress && fw.failed.as_ref().is_some_and(|f| f.contains("refused")), "{fw:?}");
    // A failure is not a lock: Install is available again.
    assert_eq!(done["available"], true, "{done}");
    assert_eq!(r.st.devices.get(&r.id).and_then(|d| d.facts).map(|f| f.reply.fw.to_string()).as_deref(), Some("0.10.0"));
}

/// Failure: the panel goes away in the middle of the upload.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panel_that_vanishes_mid_upload_is_a_reported_failure() {
    // Halfway through the image, whatever its size.
    let half = image("0.11.0").len() / 2;
    assert!(half > 0);
    let r = rig(Setup { panel: "0.10.0", offer: Some("0.11.0"), model: true, cut_after: Some(half), ..Setup::default() }).await;
    assert_eq!(r.install().await.status, 200);
    let done = r.until_settled("the failure").await;
    assert_eq!(done["run"]["phase"], "failed", "{done}");
    let why = done["run"]["why"].as_str().unwrap_or_default();
    assert!(!why.is_empty(), "it says why: {done}");
    assert_eq!(r.dev.handle().ota_phase(), screeny_sim::OtaPhase::Idle, "the image never completed, so nothing activated");
    assert_eq!(done["available"], true, "and it can be tried again: {done}");
}

/// Failure: accepted, and the panel never comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panel_that_does_not_come_back_is_a_reported_failure() {
    let r = rig(Setup { panel: "0.10.0", offer: Some("0.11.0"), model: true, gone_for_good: true, ..Setup::default() }).await;
    assert_eq!(r.install().await.status, 200);
    let done = r.until_settled("the failure").await;
    let why = done["run"]["why"].as_str().unwrap_or_default();
    assert!(why.contains("did not come back"), "{done}");
}

/// Failure: the new image is on trial and the bootloader rolls it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rolled_back_update_is_a_reported_failure() {
    let r = rig(Setup { panel: "0.10.0", offer: Some("0.11.0"), model: true, revert: true, ..Setup::default() }).await;
    assert_eq!(r.install().await.status, 200);
    let done = r.until_settled("the rollback").await;
    let why = done["run"]["why"].as_str().unwrap_or_default();
    assert!(why.contains("rolled the update back") && why.contains("0.10.0"), "{done}");
    assert_eq!(r.dev.handle().ota_phase(), screeny_sim::OtaPhase::Reverted);
    // The panel is on what it had.
    let now = get(r.studio.addr, "/api/v1/status").await.json();
    assert_eq!(now["devices"][0]["facts"]["fw"], "0.10.0", "{now}");
}

/// Failure: the panel takes the image and does not switch to it (the
/// simulator without the OTA model answers `activating: false`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_image_the_panel_will_not_switch_to_is_a_reported_failure() {
    let r = rig(Setup { panel: "0.10.0", offer: Some("0.11.0"), model: false, ..Setup::default() }).await;
    assert_eq!(r.install().await.status, 200);
    let done = r.until_settled("the failure").await;
    assert!(done["run"]["why"].as_str().unwrap_or_default().contains("would not switch"), "{done}");
}

/// The status poller leaves a panel alone while its update is running: the
/// device has one connection worker, and the update is using it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_status_poll_keeps_off_a_panel_that_is_being_updated() {
    let r = rig(Setup { panel: "0.10.0", offer: Some("0.11.0"), model: true, ..Setup::default() }).await;
    let reads_before = r.st.devices.get(&r.id).map_or(0, |d| d.http.reads);
    assert_eq!(r.install().await.status, 200);
    // A 300 ms poll would have read a dozen times in the ~1.5 s the modelled
    // update takes; the update's own reads feed the registry too, so assert
    // on what the poller must not do: run while the update is `active`.
    assert!(r.st.firmware.active(&r.id));
    let _ = r.until_settled("the update to finish").await;
    assert!(!r.st.firmware.active(&r.id));
    assert!(r.st.devices.get(&r.id).map_or(0, |d| d.http.reads) > reads_before);
}
