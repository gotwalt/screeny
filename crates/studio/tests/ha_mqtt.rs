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
use screeny_studio::Studio;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PREFIX: &str = "screeny_test";
const PATIENCE: Duration = Duration::from_secs(10);

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
    let config = seen.until_json(&config_topic, "the device config", |v| v["components"].as_object().is_some_and(|c| c.len() == 7)).await;
    assert_eq!(config["device"]["identifiers"][0], format!("screeny_{id}"));
    assert_eq!(config["origin"]["name"], "screeny-studio");
    assert_eq!(config["components"]["scene"]["options"], serde_json::json!([]));
    for entity in ["patch", "brightness", "scene", "schedule", "scheduled", "panel"] {
        seen.until(&t(entity, "state"), "a state", |_| true).await;
    }
    assert_eq!(seen.get(&t("panel", "state")).as_deref(), Some("OFF"), "no panel in a test");

    // Two modes, made on the page: the scene list follows, and so does the
    // scene that matches what is playing.
    assert_eq!(post(at, "/api/v1/set_patch", r#"{"id":"flock"}"#).await.status, 200);
    assert_eq!(post(at, "/api/v1/modes/save", r#"{"name":"Day"}"#).await.status, 200);
    assert_eq!(post(at, "/api/v1/set_patch", r#"{"id":"vesta"}"#).await.status, 200);
    assert_eq!(post(at, "/api/v1/modes/save", r#"{"name":"Night"}"#).await.status, 200);
    seen.until_json(&config_topic, "the scenes in the list", |v| v["components"]["scene"]["options"] == serde_json::json!(["Day", "Night"])).await;
    seen.until(&t("scene", "state"), "Night, which is playing", |p| p == "Night").await;

    // HA picks a scene.
    ha.publish(t("scene", "set"), QoS::AtLeastOnce, false, "Day").await.unwrap();
    seen.until_json(&t("patch", "state"), "flock", |v| v["id"] == "flock").await;
    seen.until(&t("scene", "state"), "Day", |p| p == "Day").await;
    // ...and one that is not there changes nothing.
    ha.publish(t("scene", "set"), QoS::AtLeastOnce, false, "Nope").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(seen.get(&t("patch", "state")).unwrap().contains(r#""id":"flock""#));

    // Brightness, snapped to a real stop; off is dark; a bare on comes back.
    ha.publish(t("brightness", "set"), QoS::AtLeastOnce, false, r#"{"state":"ON","brightness":100}"#).await.unwrap();
    let lit = seen.until_json(&t("brightness", "state"), "lit", |v| v["state"] == "ON").await;
    let level = lit["brightness"].as_u64().unwrap();
    assert!((90..=110).contains(&level), "the nearest stop to 100, not {level}");
    ha.publish(t("brightness", "set"), QoS::AtLeastOnce, false, r#"{"state":"OFF"}"#).await.unwrap();
    seen.until_json(&t("brightness", "state"), "dark", |v| v["state"] == "OFF").await;
    ha.publish(t("brightness", "set"), QoS::AtLeastOnce, false, r#"{"state":"ON"}"#).await.unwrap();
    seen.until_json(&t("brightness", "state"), "the same level again", |v| v["brightness"] == level).await;

    // The schedule switch.
    ha.publish(t("schedule", "set"), QoS::AtLeastOnce, false, "ON").await.unwrap();
    seen.until(&t("schedule", "state"), "on", |p| p == "ON").await;
    ha.publish(t("schedule", "set"), QoS::AtLeastOnce, false, "OFF").await.unwrap();
    seen.until(&t("schedule", "state"), "off", |p| p == "OFF").await;

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
    ha.publish(t("scene", "set"), QoS::AtLeastOnce, false, "Night").await.unwrap();
    seen.until_json(&t("patch", "state"), "vesta, after the reconnect", |v| v["id"] == "vesta").await;

    // Stopping says offline.
    studio.stop().await;
    seen.until(&status, "offline on the way out", |p| p == "offline").await;

    // And forgetting leaves nothing retained.
    screeny_studio::ha::client::forget(&mqtt, Duration::from_secs(5)).await.unwrap();
    seen.until(&config_topic, "the config removed", str::is_empty).await;
    seen.until(&status, "the status cleared", str::is_empty).await;
}
