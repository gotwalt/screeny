//! Card 302: modes and the daily schedule, over the API.
//!
//! The owner, 2026-09-26: night mode is a different patch, day comes back in
//! the morning, and a change made by hand **holds until the next timetable
//! entry**. Card 309: a mode is patch + setting and nothing else - brightness
//! is a separate concern (the Panel screen, the smart home), which a mode
//! neither sets nor is overridden by.
//!
//! The studio's clock is a hand this file holds (`Config::clock`), and the
//! scheduler looks every 100 ms instead of every 30 s, so a day passes in a
//! second. **Nothing here touches the network**: loopback, discovery off, and
//! the simulator on a port band of its own well above the spec's.

mod common;

use common::{get, post, test_config, until, until_json, Temp, PATIENCE};
use screeny_studio::schedule::{Clock, LocalNow};
use screeny_studio::{Config, Running, Studio};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use screeny_sim::{Config as SimConfig, SimDevice};

/// This file's port band.
const FIRST_PORT: u16 = 51_700;
const LAST_PORT: u16 = 51_760;

/// The studio's clock: local minutes since the epoch, moved by the test.
#[derive(Clone)]
struct Hand(Arc<AtomicI64>);

impl Hand {
    fn at(y: i64, m: u32, d: u32, hh: u16, mm: u16) -> Hand {
        let now = LocalNow::at(y, m, d, hh, mm);
        Hand(Arc::new(AtomicI64::new(now.days * 1440 + i64::from(now.minute))))
    }

    fn set(&self, y: i64, m: u32, d: u32, hh: u16, mm: u16) {
        let now = LocalNow::at(y, m, d, hh, mm);
        self.0.store(now.days * 1440 + i64::from(now.minute), Ordering::Relaxed);
    }

    fn clock(&self) -> Clock {
        let h = Arc::clone(&self.0);
        Clock::from_fn(move || {
            let m = h.load(Ordering::Relaxed);
            LocalNow { days: m.div_euclid(1440), minute: m.rem_euclid(1440) as u16 }
        })
    }
}

fn config(hand: &Hand, dir: Option<&Path>) -> Config {
    Config {
        clock: hand.clock(),
        schedule_every: Duration::from_millis(100),
        state_dir: dir.map(Path::to_path_buf),
        ..test_config()
    }
}

async fn studio_at(hand: &Hand, dir: Option<&Path>) -> Running {
    Studio::bind(config(hand, dir)).await.expect("bind an ephemeral loopback port").spawn()
}

fn start_sim() -> (SimDevice, u16) {
    for port in (FIRST_PORT..LAST_PORT).step_by(2) {
        let cfg = SimConfig {
            frame_port: port,
            control_port: port + 1,
            id: "cc5302".into(),
            instance: "sim-cc5302".into(),
            name: String::new(),
            brightness_cap: 160,
            ..SimConfig::for_test()
        };
        if let Ok(dev) = SimDevice::start(cfg) {
            return (dev, port);
        }
    }
    panic!("no free consecutive port pair in {FIRST_PORT}..{LAST_PORT}");
}

async fn ok(at: SocketAddr, path: &str, body: &str) -> serde_json::Value {
    let r = post(at, path, body).await;
    assert_eq!(r.status, 200, "{path} {body}: {}", String::from_utf8_lossy(&r.body));
    r.json()
}

/// A refusal: a 400 whose sentence ends in a full stop, like every other one.
async fn refused(at: SocketAddr, path: &str, body: &str) -> String {
    let r = post(at, path, body).await;
    assert_eq!(r.status, 400, "{path} {body} should be refused: {}", String::from_utf8_lossy(&r.body));
    let e = r.json()["error"].as_str().expect("an error sentence").to_string();
    assert!(e.ends_with('.'), "a refusal is a sentence: {e}");
    e
}

async fn state(at: SocketAddr) -> serde_json::Value {
    get(at, "/api/v1/bootstrap").await.json()["state"].clone()
}

/// Wait for the page's state to satisfy `f`; hand back the state that did.
async fn until_state(at: SocketAddr, what: &str, f: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    until_json(at, PATIENCE, what, "/api/v1/bootstrap", |b| f(&b["state"])).await["state"].clone()
}

/// **The card's acceptance, against a simulator.** Two modes, a schedule that
/// flips at 22:00 and 07:00, the studio switching patch on the minute, a hand
/// change reading `overridden` and holding, and the next entry taking it back.
/// Then a hand-applied mode, and "back to schedule". Throughout, the panel's
/// brightness is where the hand (the smart home) left it: no mode moves it,
/// and moving it is never an override (card 309).
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn night_comes_in_the_evening_and_day_in_the_morning() {
    let (sim, port) = start_sim();
    let hand = Hand::at(2026, 9, 26, 21, 58);
    let studio = studio_at(&hand, None).await;
    let at = studio.addr;
    ok(at, "/api/v1/set_panel", &format!(r#"{{"on":true,"to":"127.0.0.1:{port}"}}"#)).await;
    until(Duration::from_secs(20), "the panel to connect", || async {
        get(at, "/api/v1/panel_status").await.json()["connected"] == true
    })
    .await;

    // The smart home sets the light level; nothing below may move it.
    let b = post(at, "/api/v1/device/brightness", r#"{"device":"cc5302","level":40}"#).await;
    assert_eq!(b.status, 200, "{}", String::from_utf8_lossy(&b.body));
    let level = sim.handle().telemetry().brightness;
    assert_ne!(level, 0, "the panel is lit");

    // "Save what's playing as a mode": flock, on Default. An older client's
    // `brightness` is accepted and ignored, not refused.
    ok(at, "/api/v1/set_patch", r#"{"id":"flock"}"#).await;
    let s = ok(at, "/api/v1/modes/save", r#"{"name":"Day","brightness":100}"#).await;
    assert_eq!(s["modes"][0], serde_json::json!({"name":"Day","patch":"flock","setting":"Default"}), "patch and setting, nothing else");
    // Night: vesta's working copy.
    let s = ok(at, "/api/v1/modes/save", r#"{"name":"Night","patch":"vesta","setting":null}"#).await;
    assert_eq!(s["modes"][1], serde_json::json!({"name":"Night","patch":"vesta","setting":null}), "the working copy");
    assert_eq!(s["schedule"]["enabled"], false);
    assert_eq!(s["mode"], serde_json::Value::Null, "no schedule, no due mode");

    // The timetable. 21:58: Day is due (07:00), and is applied at once.
    let s = ok(at, "/api/v1/schedule/set", r#"{"enabled":true,"entries":[{"at":"22:00","mode":"night"},{"at":"7:00","mode":"Day"}]}"#).await;
    assert_eq!(s["schedule"]["entries"][0], serde_json::json!({"at":"07:00","mode":"Day"}), "sorted, normalised, spelled as the mode is");
    assert_eq!((s["mode"].clone(), s["until"].clone(), s["overridden"].clone()), ("Day".into(), "22:00".into(), false.into()));
    assert_eq!(s["patch"], "flock");

    // 22:00: Night, on the minute - and the light level is left alone.
    hand.set(2026, 9, 26, 22, 0);
    let s = until_state(at, "Night to come in", |s| s["patch"] == "vesta").await;
    assert_eq!((s["mode"].clone(), s["until"].clone(), s["overridden"].clone()), ("Night".into(), "07:00".into(), false.into()));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(sim.handle().telemetry().brightness, level, "a mode does not touch brightness");

    // A hand change: overridden, and it holds. (A patch neither mode plays,
    // so Day taking it back at 07:00 is visible as a change of patch.)
    let s = ok(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await;
    assert_eq!((s["overridden"].clone(), s["mode"].clone(), s["until"].clone()), (true.into(), "Night".into(), "07:00".into()));
    hand.set(2026, 9, 26, 23, 30);
    tokio::time::sleep(Duration::from_millis(500)).await;
    hand.set(2026, 9, 27, 3, 0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let s = state(at).await;
    assert_eq!(s["patch"], "metaballs", "a hand change holds until the next entry");
    assert_eq!(s["overridden"], true);

    // 07:00: the next entry takes it back.
    hand.set(2026, 9, 27, 7, 0);
    let s = until_state(at, "Day to come back", |s| s["patch"] == "flock").await;
    assert_eq!((s["mode"].clone(), s["overridden"].clone()), ("Day".into(), false.into()));
    assert_eq!(sim.handle().telemetry().brightness, level, "and still the light level is left alone");

    // A mode applied by hand (the smart home's call) is a hand change too.
    let s = ok(at, "/api/v1/mode/apply", r#"{"name":"night"}"#).await;
    assert_eq!((s["patch"].clone(), s["overridden"].clone()), ("vesta".into(), true.into()));
    hand.set(2026, 9, 27, 8, 0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state(at).await["patch"], "vesta", "it does not touch the run record, so it holds");
    // "Back to schedule".
    let s = ok(at, "/api/v1/schedule/resume", "{}").await;
    assert_eq!((s["patch"].clone(), s["mode"].clone(), s["overridden"].clone()), ("flock".into(), "Day".into(), false.into()));

    // Brightness moved by hand (the smart home, all day long) is not an override.
    let b = post(at, "/api/v1/device/brightness", r#"{"device":"cc5302","level":160}"#).await;
    assert_eq!(b.status, 200, "{}", String::from_utf8_lossy(&b.body));
    assert_eq!(state(at).await["overridden"], false, "brightness is not part of a mode");
    assert_eq!(sim.handle().telemetry().brightness, 160, "nothing pulled it back");

    // Off: nothing is due, nothing is overridden, and nothing more happens.
    let s = ok(at, "/api/v1/schedule/set", r#"{"enabled":false}"#).await;
    assert_eq!((s["mode"].clone(), s["until"].clone(), s["overridden"].clone()), (serde_json::Value::Null, serde_json::Value::Null, false.into()));
    assert_eq!(s["schedule"]["entries"].as_array().map(Vec::len), Some(2), "`enabled` alone leaves the entries");
    hand.set(2026, 9, 27, 22, 0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state(at).await["patch"], "flock", "a schedule that is off does nothing");
    assert!(refused(at, "/api/v1/schedule/resume", "{}").await.contains("off"));
    studio.stop().await;
}

/// **The hold survives a restart**, because the run record is in the file; and
/// a restart **across** an entry applies it at start-up. No panel needed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hold_survives_a_restart_and_a_restart_across_an_entry_applies_it() {
    let dir = Temp::new("schedule-restart");
    let hand = Hand::at(2026, 9, 26, 22, 5);
    let studio = studio_at(&hand, Some(&dir.0)).await;
    let at = studio.addr;
    ok(at, "/api/v1/modes/save", r#"{"name":"Day","patch":"flock","setting":null}"#).await;
    ok(at, "/api/v1/modes/save", r#"{"name":"Night","patch":"vesta","setting":"Default"}"#).await;
    let s = ok(at, "/api/v1/schedule/set", r#"{"enabled":true,"entries":[{"at":"07:00","mode":"Day"},{"at":"22:00","mode":"Night"}]}"#).await;
    assert_eq!(s["patch"], "vesta");
    // By hand, then a restart in the same stretch.
    ok(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await;
    studio.stop().await;
    let text = std::fs::read_to_string(dir.0.join("state.json")).expect("the state file");
    let file: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(file["version"], 6);
    assert_eq!(file["schedule_run"], serde_json::json!({"at":"22:00","mode":"Night","day":"2026-09-26"}));
    assert!(file.get("overridden").is_none() && file.get("until").is_none(), "computed, never stored");
    assert_eq!(file["modes"][1], serde_json::json!({"name":"Night","patch":"vesta","setting":"Default"}), "no brightness on a mode");

    hand.set(2026, 9, 27, 1, 0);
    let studio = studio_at(&hand, Some(&dir.0)).await;
    let at = studio.addr;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let s = state(at).await;
    assert_eq!(s["patch"], "metaballs", "the hold survived the restart");
    assert_eq!((s["mode"].clone(), s["overridden"].clone(), s["until"].clone()), ("Night".into(), true.into(), "07:00".into()));
    studio.stop().await;

    // Down across 07:00: the first look applies Day.
    hand.set(2026, 9, 27, 9, 30);
    let studio = studio_at(&hand, Some(&dir.0)).await;
    let at = studio.addr;
    let s = until_state(at, "Day at start-up", |s| s["patch"] == "flock").await;
    assert_eq!((s["mode"].clone(), s["overridden"].clone()), ("Day".into(), false.into()));
    studio.stop().await;
}

/// Every refusal the card names is a 400 in words, and none changes anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modes_and_schedules_are_validated() {
    let hand = Hand::at(2026, 9, 26, 12, 0);
    let studio = studio_at(&hand, None).await;
    let at = studio.addr;

    assert!(refused(at, "/api/v1/modes/save", r#"{"name":"  "}"#).await.contains("name"));
    let long = "x".repeat(41);
    refused(at, "/api/v1/modes/save", &format!(r#"{{"name":"{long}"}}"#)).await;
    assert!(refused(at, "/api/v1/modes/save", r#"{"name":"A","patch":"nonesuch"}"#).await.contains("nonesuch"));
    assert!(refused(at, "/api/v1/modes/save", r#"{"name":"A","patch":"flock","setting":"Nope"}"#).await.contains("Nope"));

    ok(at, "/api/v1/modes/save", r#"{"name":"Day","patch":"flock"}"#).await;
    ok(at, "/api/v1/modes/save", r#"{"name":"Night","patch":"vesta"}"#).await;
    // The same name in another spelling is the same mode.
    let s = ok(at, "/api/v1/modes/save", r#"{"name":"night","patch":"vesta"}"#).await;
    assert_eq!(s["modes"].as_array().map(Vec::len), Some(2));
    assert_eq!(s["modes"][1]["name"], "night");

    for bad in [
        r#"{"enabled":true,"entries":[{"at":"24:00","mode":"Day"}]}"#,
        r#"{"enabled":true,"entries":[{"at":"07:00","mode":"Nope"}]}"#,
        r#"{"enabled":true,"entries":[{"at":"07:00","mode":"Day"},{"at":"7:00","mode":"night"}]}"#,
    ] {
        refused(at, "/api/v1/schedule/set", bad).await;
    }
    let many: Vec<String> = (0..49).map(|i| format!(r#"{{"at":"{:02}:{:02}","mode":"Day"}}"#, i / 2, (i % 2) * 30)).collect();
    assert!(refused(at, "/api/v1/schedule/set", &format!(r#"{{"enabled":true,"entries":[{}]}}"#, many.join(","))).await.contains("48"));
    assert_eq!(state(at).await["schedule"]["entries"].as_array().map(Vec::len), Some(0), "a refused schedule changes nothing");

    ok(at, "/api/v1/schedule/set", r#"{"enabled":true,"entries":[{"at":"07:00","mode":"Day"},{"at":"22:00","mode":"Night"}]}"#).await;
    let e = refused(at, "/api/v1/modes/delete", r#"{"name":"NIGHT"}"#).await;
    assert!(e.contains("22:00"), "says where the schedule names it: {e}");
    // A rename carries the schedule with it.
    let s = ok(at, "/api/v1/modes/rename", r#"{"from":"night","to":"Sleep"}"#).await;
    assert_eq!(s["schedule"]["entries"][1]["mode"], "Sleep");
    assert!(refused(at, "/api/v1/modes/rename", r#"{"from":"Sleep","to":"day"}"#).await.contains("already"));
    assert!(refused(at, "/api/v1/mode/apply", r#"{"name":"Nope"}"#).await.contains("Nope"));
    // Once the schedule no longer names it, it can go.
    ok(at, "/api/v1/schedule/set", r#"{"entries":[{"at":"07:00","mode":"Day"}]}"#).await;
    let s = ok(at, "/api/v1/modes/delete", r#"{"name":"sleep"}"#).await;
    assert_eq!(s["modes"].as_array().map(Vec::len), Some(1));

    // 32 at most.
    for i in 1..32 {
        ok(at, "/api/v1/modes/save", &format!(r#"{{"name":"m{i}","patch":"flock"}}"#)).await;
    }
    assert!(refused(at, "/api/v1/modes/save", r#"{"name":"one too many","patch":"flock"}"#).await.contains("32"));
    studio.stop().await;
}

/// A mode names a **setting**: loading it is part of applying the mode,
/// moving a slider afterwards is an override, and a setting deleted since
/// falls back to the working copy - said, never an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mode_with_a_named_setting() {
    let hand = Hand::at(2026, 9, 26, 12, 0);
    let studio = studio_at(&hand, None).await;
    let at = studio.addr;
    ok(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await;
    ok(at, "/api/v1/set_param", r#"{"id":"count","value":8}"#).await;
    ok(at, "/api/v1/settings/save", r#"{"name":"Lava"}"#).await;
    // Captured: on Lava, unmodified, so Lava.
    let s = ok(at, "/api/v1/modes/save", r#"{"name":"Warm"}"#).await;
    assert_eq!(s["modes"][0]["setting"], "Lava");
    ok(at, "/api/v1/modes/save", r#"{"name":"Day","patch":"flock","setting":null}"#).await;

    ok(at, "/api/v1/set_param", r#"{"id":"count","value":3}"#).await;
    ok(at, "/api/v1/set_patch", r#"{"id":"flock"}"#).await;
    let s = ok(at, "/api/v1/schedule/set", r#"{"enabled":true,"entries":[{"at":"06:00","mode":"Warm"}]}"#).await;
    assert_eq!((s["patch"].clone(), s["setting"].clone(), s["modified"].clone()), ("metaballs".into(), "Lava".into(), false.into()));
    assert_eq!(s["params"]["count"], 8.0, "Lava, not the working copy it was left at");
    assert_eq!(s["overridden"], false);
    assert_eq!(s["until"], "06:00", "one entry: the same time tomorrow");
    let s = ok(at, "/api/v1/set_param", r#"{"id":"count","value":5}"#).await;
    assert_eq!(s["overridden"], true, "a slider moved off the mode's setting");

    // The setting goes; the mode falls back to the working copy and says so.
    ok(at, "/api/v1/settings/delete", r#"{"name":"Lava"}"#).await;
    let s = ok(at, "/api/v1/mode/apply", r#"{"name":"Warm"}"#).await;
    assert_eq!(s["patch"], "metaballs");
    let note = s["schedule_note"].as_str().expect("a note");
    assert!(note.contains("Lava"), "{note}");
    assert_eq!(s["overridden"], false, "with its setting gone, the working copy is the mode");
    studio.stop().await;
}

/// Between an entry's minute and the scheduler's next look (up to 30 s in
/// production) the entry is due and not yet applied. What is playing is then
/// not *overriding* anything - it is about to be replaced - so the page must
/// not say "Overridden" for those seconds at every change of mode (the loopback
/// walkthrough showed it for ten).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_that_has_not_been_applied_yet_is_not_an_override() {
    let hand = Hand::at(2026, 9, 26, 21, 0);
    // A scheduler that looks at start-up and then not again for an hour.
    let cfg = Config { schedule_every: Duration::from_secs(3600), ..config(&hand, None) };
    let studio = Studio::bind(cfg).await.expect("bind").spawn();
    let at = studio.addr;
    ok(at, "/api/v1/modes/save", r#"{"name":"Day","patch":"flock","setting":null}"#).await;
    ok(at, "/api/v1/modes/save", r#"{"name":"Night","patch":"vesta","setting":null}"#).await;
    let s = ok(at, "/api/v1/schedule/set", r#"{"enabled":true,"entries":[{"at":"07:00","mode":"Day"},{"at":"22:00","mode":"Night"}]}"#).await;
    assert_eq!((s["patch"].clone(), s["overridden"].clone()), ("flock".into(), false.into()));
    hand.set(2026, 9, 26, 22, 0);
    let s = state(at).await;
    assert_eq!(s["patch"], "flock", "not applied yet: the scheduler has not looked");
    assert_eq!((s["mode"].clone(), s["overridden"].clone()), ("Night".into(), false.into()));
    // A hand change after it *has* been applied is one.
    let s = ok(at, "/api/v1/schedule/resume", "{}").await;
    assert_eq!(s["patch"], "vesta");
    assert_eq!(ok(at, "/api/v1/set_patch", r#"{"id":"flock"}"#).await["overridden"], true);
    studio.stop().await;
}

/// A mode whose patch this build has not got (a hand-edited or older file) is
/// skipped when it comes due, and said - never an error, never a retry loop.
/// The file is card 302's shape, `brightness` on its mode: it loads all the
/// same (card 309).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mode_whose_patch_is_gone_is_skipped_and_said() {
    let dir = Temp::new("schedule-gone");
    std::fs::write(
        dir.0.join("state.json"),
        r#"{"version":6,"focus":"","players":[{"device":"","patch":"flock","seed":3}],
            "modes":[{"name":"Old","patch":"plasma","setting":null,"brightness":null}],
            "schedule":{"enabled":true,"entries":[{"at":"06:00","mode":"Old"}]}}"#,
    )
    .expect("write");
    let hand = Hand::at(2026, 9, 26, 12, 0);
    let studio = studio_at(&hand, Some(&dir.0)).await;
    let at = studio.addr;
    let s = until_state(at, "the skip to be said", |s| s["schedule_note"].is_string()).await;
    assert!(s["schedule_note"].as_str().is_some_and(|n| n.contains("plasma") && n.contains("skipped")), "{}", s["schedule_note"]);
    assert_eq!(s["patch"], "flock", "what was playing carries on");
    assert_eq!(s["overridden"], false, "there is nothing to be overriding");
    assert!(refused(at, "/api/v1/mode/apply", r#"{"name":"Old"}"#).await.contains("skipped"));
    assert_eq!(get(at, "/healthz").await.status, 200);
    studio.stop().await;
}
