//! Card 352: what several panels look like on the broker. Pure: a [`Fleet`]
//! in, the retained messages out, and no connection anywhere.
//!
//! **The first panel is the studio as it always was**: the same discovery id,
//! topics and `unique_id`s, and the device is named by the Settings screen's
//! Device name. Every later panel is a device of its own, `screeny_<instance>_
//! <key>`, named after the panel.

use super::payload::{self, Message};
use super::topics::{sanitize, Topics};
use super::{discovery, MqttConfig, PanelView, Snapshot};
use std::collections::HashSet;

/// What goes in a later panel's topics and ids: its device id, made safe for
/// a topic level. A device id with nothing usable in it is just `panel`.
#[must_use]
pub fn panel_key(device: &str) -> String {
    let k = sanitize(device);
    if k.is_empty() {
        "panel".to_string()
    } else {
        k
    }
}

/// The keys for `devices` in adoption order: `None` for the first, then each
/// one's [`panel_key`], made unique (`_2`, `_3`, ...) if two collide.
#[must_use]
pub fn keys(devices: &[String]) -> Vec<Option<String>> {
    let mut seen = HashSet::new();
    devices
        .iter()
        .enumerate()
        .map(|(i, d)| {
            if i == 0 {
                return None;
            }
            let base = panel_key(d);
            let mut k = base.clone();
            let mut n = 2;
            while !seen.insert(k.clone()) {
                k = format!("{base}_{n}");
                n += 1;
            }
            Some(k)
        })
        .collect()
}

/// One panel's HA device, as its own connection settings: the first keeps
/// the name from the Settings screen, a later one is named after its panel.
#[must_use]
pub fn device_cfg(cfg: &MqttConfig, view: &PanelView) -> MqttConfig {
    if view.key.is_some() && !view.name.trim().is_empty() {
        MqttConfig { name: view.name.clone(), ..cfg.clone() }
    } else {
        cfg.clone()
    }
}

#[must_use]
pub fn topics_of(cfg: &MqttConfig, view: &PanelView) -> Topics {
    Topics::for_panel(cfg, view.key.as_deref())
}

/// Every retained message for `fleet`, in the order HA needs: a panel's
/// config before its states, and `online` after the first config and before
/// any state. With no panel at all, only `online`.
#[must_use]
pub fn announce(cfg: &MqttConfig, fleet: &[PanelView]) -> Vec<Message> {
    let online = || Message { topic: Topics::new(cfg).status, payload: super::client::ONLINE.to_string() };
    let mut out = Vec::new();
    if fleet.is_empty() {
        out.push(online());
    }
    for (i, view) in fleet.iter().enumerate() {
        let dcfg = device_cfg(cfg, view);
        let topics = topics_of(&dcfg, view);
        out.push(Message { topic: topics.discovery.clone(), payload: discovery::payload(&dcfg, &topics, &view.snapshot).to_string() });
        if i == 0 {
            out.push(online());
        }
        out.extend(payload::state_messages(&topics, &view.snapshot));
    }
    out
}

/// Every topic a panel's device retains, to be cleared with an empty payload
/// when the panel goes: its discovery config and each state.
#[must_use]
pub fn retained_topics(topics: &Topics) -> Vec<String> {
    let mut out = vec![topics.discovery.clone()];
    out.extend(payload::state_messages(topics, &Snapshot::default()).into_iter().map(|m| m.topic));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::payload::tests::{config, pictures, snapshot_json};
    use crate::ha::{Fleet, PanelView, PatchState};

    fn snap(setting: &str) -> Snapshot {
        Snapshot {
            patch: PatchState { id: "overland".into(), name: "Overland".into(), setting: setting.into(), modified: false },
            brightness: Some(96),
            pictures: pictures(),
            picture: Some(format!("Overland · {setting}")),
            panel_connected: true,
        }
    }

    fn view(device: &str, key: Option<&str>, name: &str) -> PanelView {
        PanelView { device: device.into(), key: key.map(str::to_string), name: name.into(), snapshot: snap("Dusk") }
    }

    fn one(name: &str) -> Fleet {
        vec![view("screeny-4a00a4", None, name)]
    }

    /// **The first panel is byte-for-byte what main publishes.** The two
    /// snapshot files were written by the single-device build (cards 308-311)
    /// and are compared as they are; the literals below are the topics and
    /// ids the owner's automations name.
    #[test]
    fn the_first_panel_is_exactly_what_a_single_device_studio_says() {
        let cfg = config();
        let out = announce(&cfg, &one("A name from the panel, which the first panel ignores"));
        let (config_msg, rest) = (&out[0], &out[1..]);
        assert_eq!(config_msg.topic, "homeassistant/device/screeny_studio/config");
        let text = serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(&config_msg.payload).unwrap())
            .unwrap()
            .replace(env!("CARGO_PKG_VERSION"), "VERSION");
        // The discovery snapshot is of `snap()` in discovery.rs: the four
        // pictures and a default patch; this one has a patch playing, which
        // the config does not carry.
        snapshot_json("discovery.json", &text);
        assert_eq!((rest[0].topic.as_str(), rest[0].payload.as_str()), ("screeny/studio/status", "online"));
        let states: String = rest[1..].iter().map(|m| format!("{} {}", m.topic, m.payload)).collect::<Vec<_>>().join("\n");
        snapshot_json("state-lit.txt", &states);

        // Every id and topic, spelt out.
        let v: serde_json::Value = serde_json::from_str(&config_msg.payload).unwrap();
        assert_eq!(v["device"]["identifiers"], serde_json::json!(["screeny_studio"]));
        assert_eq!(v["device"]["name"], "Screeny");
        let mut ids: Vec<String> = ["brightness", "level", "picture", "patch", "panel"].iter().map(|k| v["components"][k]["unique_id"].as_str().unwrap().to_string()).collect();
        ids.sort();
        assert_eq!(ids, ["screeny_studio_brightness", "screeny_studio_level", "screeny_studio_panel", "screeny_studio_patch", "screeny_studio_picture"]);
        let topics: Vec<&str> = rest[1..].iter().map(|m| m.topic.as_str()).collect();
        assert_eq!(
            topics,
            ["screeny/studio/patch/state", "screeny/studio/brightness/state", "screeny/studio/level/state", "screeny/studio/picture/state", "screeny/studio/panel/state"]
        );
        for retired in ["scene", "schedule", "resume", "scheduled"] {
            assert!(v["components"][retired]["platform"].is_string(), "the first panel still removes {retired}");
        }
    }

    #[test]
    fn a_later_panel_is_its_own_device_named_after_the_panel() {
        let cfg = config();
        let fleet = vec![view("aa0001", None, "Living room"), view("screeny-4a00a5", Some("screeny-4a00a5"), "Kitchen")];
        let out = announce(&cfg, &fleet);
        let topics: Vec<&str> = out.iter().map(|m| m.topic.as_str()).collect();
        assert_eq!(topics[0], "homeassistant/device/screeny_studio/config");
        assert_eq!(topics[1], "screeny/studio/status");
        assert_eq!(topics[7], "homeassistant/device/screeny_studio_screeny-4a00a5/config");
        assert_eq!(topics[8], "screeny/studio/screeny-4a00a5/patch/state");
        assert_eq!(topics.iter().filter(|t| t.ends_with("/status")).count(), 1, "one availability for the studio");
        let second: serde_json::Value = serde_json::from_str(&out[7].payload).unwrap();
        assert_eq!(second["device"]["identifiers"], serde_json::json!(["screeny_studio_screeny-4a00a5"]));
        assert_eq!(second["device"]["name"], "Kitchen");
        assert_eq!(second["components"]["picture"]["unique_id"], "screeny_studio_screeny-4a00a5_picture");
        assert_eq!(second["components"]["picture"]["command_topic"], "screeny/studio/screeny-4a00a5/picture/set");
        assert_eq!(second["components"]["picture"]["availability_topic"], "screeny/studio/status");
        assert_eq!(second["components"].as_object().unwrap().len(), 5, "no retired removals on a device that never had them");
        let first: serde_json::Value = serde_json::from_str(&out[0].payload).unwrap();
        assert_eq!(first["device"]["name"], "Screeny", "the first is named by the Settings screen, as before");
    }

    #[test]
    fn a_rename_changes_only_the_later_panels_device_name() {
        let cfg = config();
        let before = announce(&cfg, &[view("a", None, "One"), view("b", Some("b"), "Kitchen")]);
        let after = announce(&cfg, &[view("a", None, "Uno"), view("b", Some("b"), "Pantry")]);
        assert_eq!(before[0], after[0], "the first panel's config does not follow its name");
        let (b, a) = (before.iter().find(|m| m.topic.ends_with("_b/config")).unwrap(), after.iter().find(|m| m.topic.ends_with("_b/config")).unwrap());
        assert_ne!(b.payload, a.payload);
        assert!(a.payload.contains(r#""name":"Pantry""#));
    }

    #[test]
    fn keys_are_first_none_then_safe_and_unique() {
        let devices: Vec<String> = ["aa0001", "bb0002", "bb 0002", "///", "addr:192.168.7.9"].map(String::from).into();
        assert_eq!(keys(&devices), [None, Some("bb0002".into()), Some("bb_0002".into()), Some("panel".into()), Some("addr_192_168_7_9".into())]);
        let same: Vec<String> = ["a", "x y", "x/y"].map(String::from).into();
        assert_eq!(keys(&same), [None, Some("x_y".into()), Some("x_y_2".into())]);
        assert_eq!(keys(&[]), Vec::<Option<String>>::new());
    }

    #[test]
    fn what_a_forgotten_panel_clears() {
        let cfg = config();
        let t = Topics::for_panel(&cfg, Some("b"));
        let topics = retained_topics(&t);
        assert_eq!(topics[0], "homeassistant/device/screeny_studio_b/config");
        assert!(topics[1..].iter().all(|t| t.starts_with("screeny/studio/b/")), "{topics:?}");
        assert_eq!(topics.len(), 6);
    }
}
