//! Card 308: Home Assistant over MQTT, against a real broker.
//!
//! **Ignored by default**: it needs a broker, and nothing else in this crate
//! reaches outside loopback. Run it against a throwaway Mosquitto:
//!
//! ```text
//! docker run -d --rm --name screeny-mqtt -p 127.0.0.1:18830:1883 \
//!     eclipse-mosquitto:2 mosquitto -c /mosquitto-no-auth.conf
//! SCREENY_TEST_MQTT=127.0.0.1:18830 cargo test -p screeny-studio --test ha_mqtt -- --ignored
//! docker stop screeny-mqtt
//! ```
//!
//! It uses a discovery prefix of its own (`screeny_test`), so even pointed at
//! the house broker it cannot put entities in front of a real Home Assistant,
//! and it removes everything it retained on the way out.

mod common;

use common::{post, test_config};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use screeny_studio::ha::MqttConfig;
use screeny_sim::{Config as SimConfig, SimDevice};
use screeny_studio::Studio;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PREFIX: &str = "screeny_test";
const PATIENCE: Duration = Duration::from_secs(10);
// (A studio started with `Config::mqtt` begins connected, as if the Settings
// screen had been filled in; the rest of this file goes through that screen's
// routes.)

fn broker() -> (String, u16) {
    let at = std::env::var("SCREENY_TEST_MQTT").unwrap_or_else(|_| "127.0.0.1:1883".into());
    let (host, port) = at.rsplit_once(':').expect("SCREENY_TEST_MQTT is HOST:PORT");
    (host.to_string(), port.parse().expect("a port"))
}

/// Everything seen on the broker, newest payload per topic.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<HashMap<String, String>>>);

impl Seen {
    fn get(&self, topic: &str) -> Option<String> {
        self.0.lock().unwrap().get(topic).cloned()
    }

    fn forget(&self, topic: &str) {
        self.0.lock().unwrap().remove(topic);
    }

    async fn until(&self, topic: &str, what: &str, want: impl Fn(&str) -> bool) -> String {
        let start = tokio::time::Instant::now();
        loop {
            if let Some(p) = self.get(topic) {
                if want(&p) {
                    return p;
                }
            }
            assert!(start.elapsed() < PATIENCE, "{topic}: waited {PATIENCE:?} for {what}; last saw {:?}", self.get(topic));
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn until_json(&self, topic: &str, what: &str, want: impl Fn(&Value) -> bool) -> Value {
        let p = self.until(topic, what, |p| serde_json::from_str(p).is_ok_and(|v| want(&v))).await;
        serde_json::from_str(&p).unwrap()
    }
}

/// A client that plays Home Assistant: it watches every topic of ours and
/// publishes commands.
async fn home_assistant(host: &str, port: u16, id: &str) -> (AsyncClient, Seen) {
    let mut opts = MqttOptions::new(format!("ha-{id}"), host, port);
    opts.set_max_packet_size(64 * 1024, 64 * 1024);
    let (client, mut eventloop) = AsyncClient::new(opts, 32);
    client.subscribe(format!("screeny/{id}/#"), QoS::AtLeastOnce).await.unwrap();
    client.subscribe(format!("{PREFIX}/device/screeny_{id}/config"), QoS::AtLeastOnce).await.unwrap();
    // Card 352: every later panel's device.
    client.subscribe(format!("{PREFIX}/device/+/config"), QoS::AtLeastOnce).await.unwrap();
    let seen = Seen::default();
    let s = seen.clone();
    tokio::spawn(async move {
        while let Ok(ev) = eventloop.poll().await {
            if let Event::Incoming(Packet::Publish(p)) = ev {
                s.0.lock().unwrap().insert(p.topic, String::from_utf8_lossy(&p.payload).into_owned());
            }
        }
    });
    (client, seen)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a broker: SCREENY_TEST_MQTT=HOST:PORT (see the top of this file)"]
async fn home_assistant_sees_and_drives_the_studio() {
    let (host, port) = broker();
    let id = format!("test_{}", std::process::id());
    let mqtt = MqttConfig {
        host: host.clone(),
        port,
        username: None,
        password: None,
        discovery_prefix: PREFIX.into(),
        instance: id.clone(),
        name: "Screeny test".into(),
    };
    let t = |entity: &str, kind: &str| format!("screeny/{id}/{entity}/{kind}");
    let config_topic = format!("{PREFIX}/device/screeny_{id}/config");
    let status = format!("screeny/{id}/status");

    let (ha, seen) = home_assistant(&host, port, &id).await;
    let studio = Studio::bind(screeny_studio::Config { mqtt: Some(mqtt.clone()), ..test_config() }).await.unwrap().spawn();
    let at = studio.addr;

    // Connect: the config, online, and every state.
    seen.until(&status, "online", |p| p == "online").await;
    let config = seen.until_json(&config_topic, "the device config", |v| v["components"].as_object().is_some_and(|c| c.len() == 9)).await;
    assert_eq!(config["device"]["identifiers"][0], format!("screeny_{id}"));
    assert_eq!(config["origin"]["name"], "screeny-studio");
    for (retired, platform) in [("scene", "select"), ("schedule", "switch"), ("resume", "button"), ("scheduled", "sensor")] {
        assert_eq!(config["components"][retired], serde_json::json!({ "platform": platform }), "card 310 removes {retired}");
    }
    let options = config["components"]["picture"]["options"].as_array().expect("the picture list").clone();
    assert!(options.contains(&"Flock".into()) && options.contains(&"Vesta".into()), "{options:?}");
    for entity in ["patch", "brightness", "level", "picture", "panel"] {
        seen.until(&t(entity, "state"), "a state", |_| true).await;
    }
    assert_eq!(seen.get(&t("panel", "state")).as_deref(), Some("OFF"), "no panel in a test");

    // HA picks a picture: a patch on Default is its bare name.
    ha.publish(t("picture", "set"), QoS::AtLeastOnce, false, "Flock").await.unwrap();
    seen.until_json(&t("patch", "state"), "flock", |v| v["id"] == "flock").await;
    seen.until(&t("picture", "state"), "Flock", |p| p == "Flock").await;
    // A setting saved on the page joins the list, and can be picked.
    assert_eq!(post(at, "/api/v1/set_param", r#"{"id":"birds","value":40}"#).await.status, 200);
    seen.until(&t("picture", "state"), "unknown once a slider moves", |p| p == "None").await;
    assert_eq!(post(at, "/api/v1/settings/save", r#"{"name":"Busy"}"#).await.status, 200);
    seen.until_json(&config_topic, "Flock · Busy in the list", |v| {
        v["components"]["picture"]["options"].as_array().is_some_and(|o| o.contains(&"Flock · Busy".into()))
    })
    .await;
    seen.until(&t("picture", "state"), "Flock · Busy", |p| p == "Flock · Busy").await;
    ha.publish(t("picture", "set"), QoS::AtLeastOnce, false, "Vesta").await.unwrap();
    seen.until_json(&t("patch", "state"), "vesta", |v| v["id"] == "vesta").await;
    ha.publish(t("picture", "set"), QoS::AtLeastOnce, false, "Flock · Busy").await.unwrap();
    seen.until_json(&t("patch", "state"), "flock on Busy", |v| v["id"] == "flock" && v["setting"] == "Busy").await;
    // ...and one that is not there changes nothing.
    ha.publish(t("picture", "set"), QoS::AtLeastOnce, false, "Nope").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(seen.get(&t("patch", "state")).unwrap().contains(r#""setting":"Busy""#));

    // Brightness, snapped to a real stop; off is dark; a bare on comes back.
    ha.publish(t("brightness", "set"), QoS::AtLeastOnce, false, r#"{"state":"ON","brightness":100}"#).await.unwrap();
    let lit = seen.until_json(&t("brightness", "state"), "lit", |v| v["state"] == "ON").await;
    let level = lit["brightness"].as_u64().unwrap();
    assert!((90..=110).contains(&level), "the nearest stop to 100, not {level}");
    seen.until(&t("level", "state"), "the slider follows", |p| p == "40").await;
    ha.publish(t("brightness", "set"), QoS::AtLeastOnce, false, r#"{"state":"OFF"}"#).await.unwrap();
    seen.until_json(&t("brightness", "state"), "dark", |v| v["state"] == "OFF").await;
    seen.until(&t("level", "state"), "the slider at 0", |p| p == "0").await;
    ha.publish(t("brightness", "set"), QoS::AtLeastOnce, false, r#"{"state":"ON"}"#).await.unwrap();
    seen.until_json(&t("brightness", "state"), "the same level again", |v| v["brightness"] == level).await;
    // The slider, in the panel's own 4 % steps.
    ha.publish(t("level", "set"), QoS::AtLeastOnce, false, "8").await.unwrap();
    seen.until(&t("level", "state"), "8 %", |p| p == "8").await;
    seen.until_json(&t("brightness", "state"), "the light follows", |v| v["state"] == "ON" && v["brightness"] == 16).await;

    // HA restarts: its birth message brings the config back.
    seen.forget(&config_topic);
    ha.publish(format!("{PREFIX}/status"), QoS::AtLeastOnce, false, "online").await.unwrap();
    seen.until(&config_topic, "the config again", |p| !p.is_empty()).await;

    // The broker drops the studio - here, another client taking its id. The
    // Will says offline; the studio reconnects, resubscribes and says it all
    // again, and a command still works.
    seen.forget(&t("patch", "state"));
    let (thief, mut thief_loop) = AsyncClient::new(MqttOptions::new(format!("screeny-{id}"), host.as_str(), port), 8);
    while !matches!(thief_loop.poll().await, Ok(Event::Incoming(Packet::ConnAck(_)))) {}
    seen.until(&status, "the Will", |p| p == "offline").await;
    thief.disconnect().await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(1), thief_loop.poll()).await;
    seen.until(&status, "online again", |p| p == "online").await;
    seen.until(&t("patch", "state"), "every state again", |_| true).await;
    ha.publish(t("picture", "set"), QoS::AtLeastOnce, false, "Vesta").await.unwrap();
    seen.until_json(&t("patch", "state"), "vesta, after the reconnect", |v| v["id"] == "vesta").await;

    // Card 311: the Settings screen. What it is told never has the password;
    // switching off says offline, and on again brings it all back.
    let view = common::get(at, "/api/v1/home_assistant").await.json();
    assert_eq!(view["status"]["state"], "connected", "{view}");
    assert_eq!(view["password_set"], false);
    assert!(view.get("password").is_none(), "{view}");
    let off = post(at, "/api/v1/home_assistant/set", r#"{"enabled":false}"#).await.json();
    assert_eq!(off["enabled"], false);
    seen.until(&status, "offline when switched off", |p| p == "offline").await;
    let on = post(at, "/api/v1/home_assistant/set", r#"{"enabled":true}"#).await.json();
    assert_eq!(on["enabled"], true);
    seen.until(&status, "online when switched on", |p| p == "online").await;
    assert_eq!(post(at, "/api/v1/home_assistant/set", r#"{"enabled":true,"host":""}"#).await.status, 400, "on, but nowhere");

    // "Remove from Home Assistant": off, and nothing retained.
    let gone = post(at, "/api/v1/home_assistant/forget", "{}").await;
    assert_eq!(gone.status, 200, "{}", String::from_utf8_lossy(&gone.body));
    assert_eq!(gone.json()["enabled"], false);
    seen.until(&config_topic, "the config removed", str::is_empty).await;
    seen.until(&status, "the status cleared", str::is_empty).await;
    studio.stop().await;
}

// ------------------------------------------------------------ card 352 ---

/// A simulator on consecutive loopback ports (a typed `IP:PORT` takes the
/// control port to be frame + 1), in a band of its own.
fn start_sim(id: &str, avoid: u16) -> (SimDevice, u16) {
    for port in (51_200u16..51_300).step_by(2) {
        if port == avoid {
            continue;
        }
        let cfg = SimConfig { frame_port: port, control_port: port + 1, id: id.to_string(), instance: format!("sim-{id}"), ..SimConfig::for_test() };
        if let Ok(dev) = SimDevice::start_with(cfg, None) {
            return (dev, port);
        }
    }
    panic!("no free consecutive port pair in 51200..51300");
}

/// The channel each panel is on, from the overview.
async fn channels(at: std::net::SocketAddr) -> (Value, Value) {
    let v = common::get(at, "/api/v1/panels").await.json();
    let of = |d: &str| v["panels"].as_array().unwrap().iter().find(|p| p["device"] == d).unwrap_or_else(|| panic!("no {d} in {v}"))["channel"].clone();
    (of("aa0001"), of("bb0002"))
}

/// **Card 352: one HA device per panel**, against a real broker and two sims.
///
/// The first panel is the device every earlier build announced; the second is
/// its own device named after the panel; a pick on one does not touch the
/// other; the same picture on both is one channel; renaming a panel renames
/// its device; forgetting it removes the device.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a broker: SCREENY_TEST_MQTT=HOST:PORT (see the top of this file)"]
async fn every_panel_is_an_ha_device() {
    let (host, port) = broker();
    let id = format!("multi_{}", std::process::id());
    let mqtt = MqttConfig {
        host: host.clone(),
        port,
        username: None,
        password: None,
        discovery_prefix: PREFIX.into(),
        instance: id.clone(),
        name: "Screeny test".into(),
    };
    let one = |entity: &str, kind: &str| format!("screeny/{id}/{entity}/{kind}");
    let two = |entity: &str, kind: &str| format!("screeny/{id}/bb0002/{entity}/{kind}");
    let first_config = format!("{PREFIX}/device/screeny_{id}/config");
    let second_config = format!("{PREFIX}/device/screeny_{id}_bb0002/config");

    let (ha, seen) = home_assistant(&host, port, &id).await;
    let (_a_dev, a_port) = start_sim("aa0001", 0);
    let (_b_dev, b_port) = start_sim("bb0002", a_port);
    let studio = Studio::bind(screeny_studio::Config { mqtt: Some(mqtt), ..test_config() }).await.unwrap().spawn();
    let at = studio.addr;
    for p in [a_port, b_port] {
        assert_eq!(post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{p}","play":true}}"#)).await.status, 200);
    }
    common::until_json(at, PATIENCE, "both panels", "/api/v1/panels", |p| {
        p["panels"].as_array().is_some_and(|a| a.len() == 2 && a[0]["device"] == "aa0001" && a[1]["device"] == "bb0002")
    })
    .await;

    // Two devices. The first is exactly the one a single-panel studio made.
    let first = seen.until_json(&first_config, "the first panel's config", |v| v["components"].as_object().is_some_and(|c| c.len() == 9)).await;
    assert_eq!(first["device"]["identifiers"], serde_json::json!([format!("screeny_{id}")]));
    assert_eq!(first["device"]["name"], "Screeny test");
    for k in ["brightness", "level", "picture", "patch", "panel"] {
        assert_eq!(first["components"][k]["unique_id"], format!("screeny_{id}_{k}"));
    }
    assert_eq!(first["components"]["picture"]["command_topic"], one("picture", "set"));
    let second = seen.until_json(&second_config, "the second panel's config", |v| v["components"].as_object().is_some()).await;
    assert_eq!(second["device"]["identifiers"], serde_json::json!([format!("screeny_{id}_bb0002")]));
    assert_eq!(second["device"]["name"], "screeny sim", "named after the panel: the name the device says (a sim's is this)");
    assert_eq!(second["components"].as_object().unwrap().len(), 5);
    for k in ["brightness", "level", "picture", "patch", "panel"] {
        assert_eq!(second["components"][k]["unique_id"], format!("screeny_{id}_bb0002_{k}"));
    }
    assert_eq!(second["components"]["picture"]["command_topic"], two("picture", "set"));
    assert_eq!(second["components"]["picture"]["availability_topic"], format!("screeny/{id}/status"));
    for entity in ["patch", "brightness", "level", "picture", "panel"] {
        seen.until(&one(entity, "state"), "the first panel's state", |_| true).await;
        seen.until(&two(entity, "state"), "the second panel's state", |_| true).await;
    }

    // A pick on panel 2 leaves panel 1 alone: different pictures, two channels.
    ha.publish(one("picture", "set"), QoS::AtLeastOnce, false, "Flock").await.unwrap();
    seen.until_json(&one("patch", "state"), "flock on the first", |v| v["id"] == "flock").await;
    ha.publish(two("picture", "set"), QoS::AtLeastOnce, false, "Vesta").await.unwrap();
    seen.until_json(&two("patch", "state"), "vesta on the second", |v| v["id"] == "vesta").await;
    seen.until(&two("picture", "state"), "Vesta", |p| p == "Vesta").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(seen.get(&one("patch", "state")).unwrap().contains(r#""id":"flock""#), "panel 1 is untouched");
    let (a, b) = channels(at).await;
    assert!(a.is_number() && b.is_number() && a != b, "two channels: {a} {b}");

    // The same picture on both: one channel, by the page's own rules.
    ha.publish(one("picture", "set"), QoS::AtLeastOnce, false, "Vesta").await.unwrap();
    seen.until_json(&one("patch", "state"), "vesta on the first", |v| v["id"] == "vesta").await;
    let (a, b) = channels(at).await;
    assert_eq!(a, b, "one channel for one picture");

    // Brightness is per panel.
    ha.publish(two("level", "set"), QoS::AtLeastOnce, false, "8").await.unwrap();
    seen.until(&two("level", "state"), "8 % on the second", |p| p == "8").await;
    assert_ne!(seen.get(&one("level", "state")).as_deref(), Some("8"), "the first is not dimmed");

    // Renaming a panel renames its device (the first is named on the Settings
    // screen, and does not follow).
    assert_eq!(post(at, "/api/v1/devices/add", &format!(r#"{{"to":"127.0.0.1:{b_port}","name":"Kitchen"}}"#)).await.status, 200);
    let renamed = seen.until_json(&second_config, "the new name", |v| v["device"]["name"] == "Kitchen").await;
    assert_eq!(renamed["device"]["identifiers"], second["device"]["identifiers"], "same device, new name");

    // Forgetting a panel removes its device and clears its states; the first
    // is not touched.
    assert_eq!(post(at, "/api/v1/devices/forget", r#"{"device":"bb0002"}"#).await.status, 200);
    seen.until(&second_config, "the second config removed", str::is_empty).await;
    seen.until(&two("patch", "state"), "its states cleared", str::is_empty).await;
    assert!(!seen.get(&first_config).unwrap().is_empty());
    assert!(!seen.get(&one("patch", "state")).unwrap().is_empty());

    // "Remove from Home Assistant" clears the first as well.
    assert_eq!(post(at, "/api/v1/home_assistant/forget", "{}").await.status, 200);
    seen.until(&first_config, "the first config removed", str::is_empty).await;
    studio.stop().await;
}
