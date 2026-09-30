//! Cards 352, 355: what several panels and channels look like on the broker.
//! Pure: a [`Fleet`] in, the retained messages out, and no connection anywhere.
//!
//! **The first panel is the studio as it always was**: the same discovery id,
//! topics and `unique_id`s, and the device is named by the Settings screen's
//! Device name. Its picture and patch are now Channel 1's, and it has a
//! channel select. Every later panel is a device of its own, `screeny_<instance>_
//! <key>`, named after the panel, with a channel select and no picture. Every
//! channel but Channel 1 is a device of its own, `screeny_<instance>_ch<id>`,
//! named after the channel, with the picture and the patch.

use super::payload::{self, Message};
use super::topics::{sanitize, Topics};
use super::topics::is_channel_key;
use super::{discovery, ChannelView, Fleet, MqttConfig, PanelView, Snapshot};
use std::collections::HashSet;

/// What goes in a later panel's topics and ids: its device id, made safe for
/// a topic level. A device id with nothing usable in it is just `panel`.
#[must_use]
pub fn panel_key(device: &str) -> String {
    let k = sanitize(device);
    if k.is_empty() {
        "panel".to_string()
    } else if is_channel_key(&k) {
        // `ch<id>` is where a channel's topics live.
        format!("{k}_panel")
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

/// A channel's HA device, named after the channel.
#[must_use]
pub fn channel_cfg(cfg: &MqttConfig, view: &ChannelView) -> MqttConfig {
    MqttConfig { name: view.name.clone(), ..cfg.clone() }
}

/// The channels that have a device of their own: all but Channel 1, whose
/// picture the first panel's device carries.
pub fn channel_devices(fleet: &Fleet) -> impl Iterator<Item = &ChannelView> {
    fleet.channels.iter().filter(|c| c.id != crate::state::HOME_CHANNEL)
}

/// Every retained message for `fleet`, in the order HA needs: a panel's
/// config before its states, and `online` after the first config and before
/// any state. With no panel at all, only `online`.
#[must_use]
pub fn announce(cfg: &MqttConfig, fleet: &Fleet) -> Vec<Message> {
    let online = || Message { topic: Topics::new(cfg).status, payload: super::client::ONLINE.to_string() };
    let mut out = Vec::new();
    if fleet.panels.is_empty() {
        out.push(online());
    }
    for (i, view) in fleet.panels.iter().enumerate() {
        let dcfg = device_cfg(cfg, view);
        let topics = topics_of(&dcfg, view);
        out.push(Message { topic: topics.discovery.clone(), payload: discovery::payload(&dcfg, &topics, &view.snapshot).to_string() });
        if i == 0 {
            out.push(online());
        }
        out.extend(payload::state_messages(&topics, &view.snapshot));
    }
    for view in channel_devices(fleet) {
        let dcfg = channel_cfg(cfg, view);
        let topics = Topics::for_channel(cfg, view.id);
        out.push(Message { topic: topics.discovery.clone(), payload: discovery::payload(&dcfg, &topics, &view.snapshot).to_string() });
        out.extend(payload::state_messages(&topics, &view.snapshot));
    }
    out
}

/// The topics of every device the fleet has now: panels, then channels.
#[must_use]
pub fn devices(cfg: &MqttConfig, fleet: &Fleet) -> Vec<Topics> {
    let mut out: Vec<Topics> = fleet.panels.iter().map(|v| topics_of(cfg, v)).collect();
    out.extend(channel_devices(fleet).map(|c| Topics::for_channel(cfg, c.id)));
    out
}

/// Every topic a device retains, to be cleared with an empty payload when the
/// panel or channel goes: its discovery config and each state - a later
/// panel's old picture and patch included, which card 355 took from it.
#[must_use]
pub fn retained_topics(topics: &Topics) -> Vec<String> {
    let mut out = vec![topics.discovery.clone()];
    out.extend(payload::state_messages(topics, &Snapshot::default()).into_iter().map(|m| m.topic));
    for t in topics.retired_state_topics() {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::payload::tests::{config, pictures, snapshot_json};
    use crate::ha::{ChannelOption, Fleet, PanelView, PatchState};

    fn options() -> Vec<ChannelOption> {
        ChannelOption::all(&[(1, "Channel 1".into()), (2, "Drawing room".into())])
    }

    fn snap(setting: &str, channel: u32) -> Snapshot {
        Snapshot {
            patch: PatchState { id: "overland".into(), name: "Overland".into(), setting: setting.into(), modified: false },
            brightness: Some(96),
            pictures: pictures(),
            picture: Some(format!("Overland · {setting}")),
            panel_connected: true,
            channels: options(),
            channel: Some(channel),
        }
    }

    fn view(device: &str, key: Option<&str>, name: &str) -> PanelView {
        PanelView { device: device.into(), key: key.map(str::to_string), name: name.into(), snapshot: snap("Dusk", 1) }
    }

    fn channel(id: u32, name: &str) -> ChannelView {
        ChannelView { id, name: name.into(), snapshot: Snapshot { brightness: None, ..snap("Dusk", id) } }
    }

    fn fleet(panels: Vec<PanelView>) -> Fleet {
        Fleet { panels, channels: vec![channel(1, "Channel 1"), channel(2, "Drawing room")] }
    }

    fn one(name: &str) -> Fleet {
        fleet(vec![view("screeny-4a00a4", None, name)])
    }

    fn json(m: &Message) -> serde_json::Value {
        serde_json::from_str(&m.payload).unwrap()
    }

    /// **The first panel is byte-for-byte what main publishes.** The two
    /// snapshot files were written by the single-device build (cards 308-311)
    /// and are compared as they are, **with card 355's one addition - the
    /// channel select - taken out of the config and off the end of the
    /// states**; the literals below are the topics and ids the owner's
    /// automations name. `select.screeny_picture` and `sensor.screeny_patch`
    /// are now Channel 1's, which is what the first device's snapshot
    /// carries.
    #[test]
    fn the_first_panel_is_exactly_what_a_single_device_studio_says() {
        let cfg = config();
        let out = announce(&cfg, &one("A name from the panel, which the first panel ignores"));
        let (config_msg, rest) = (&out[0], &out[1..]);
        assert_eq!(config_msg.topic, "homeassistant/device/screeny_studio/config");
        let mut v = json(config_msg);
        let channel_select = v["components"].as_object_mut().unwrap().remove("channel").unwrap();
        assert_eq!(channel_select["unique_id"], "screeny_studio_channel");
        assert_eq!(channel_select["command_topic"], "screeny/studio/channel/set");
        assert_eq!(channel_select["options"], serde_json::json!(["Channel 1", "Drawing room"]));
        let text = serde_json::to_string_pretty(&v).unwrap().replace(env!("CARGO_PKG_VERSION"), "VERSION");
        // The discovery snapshot is of `snap()` in discovery.rs: the four
        // pictures and a default patch; this one has a patch playing, which
        // the config does not carry.
        snapshot_json("discovery.json", &text);
        assert_eq!((rest[0].topic.as_str(), rest[0].payload.as_str()), ("screeny/studio/status", "online"));
        let states: String = rest[1..6].iter().map(|m| format!("{} {}", m.topic, m.payload)).collect::<Vec<_>>().join("\n");
        snapshot_json("state-lit.txt", &states);
        assert_eq!((rest[6].topic.as_str(), rest[6].payload.as_str()), ("screeny/studio/channel/state", "Channel 1"), "the one addition, last");

        // Every id and topic, spelt out.
        let v = json(config_msg);
        assert_eq!(v["device"]["identifiers"], serde_json::json!(["screeny_studio"]));
        assert_eq!(v["device"]["name"], "Screeny");
        let mut ids: Vec<String> =
            ["brightness", "level", "picture", "patch", "panel"].iter().map(|k| v["components"][k]["unique_id"].as_str().unwrap().to_string()).collect();
        ids.sort();
        assert_eq!(ids, ["screeny_studio_brightness", "screeny_studio_level", "screeny_studio_panel", "screeny_studio_patch", "screeny_studio_picture"]);
        assert_eq!(v["components"]["picture"]["command_topic"], "screeny/studio/picture/set");
        assert_eq!(v["components"]["picture"]["state_topic"], "screeny/studio/picture/state");
        assert_eq!(v["components"]["patch"]["state_topic"], "screeny/studio/patch/state");
        let topics: Vec<&str> = rest[1..7].iter().map(|m| m.topic.as_str()).collect();
        assert_eq!(
            topics,
            [
                "screeny/studio/patch/state",
                "screeny/studio/brightness/state",
                "screeny/studio/level/state",
                "screeny/studio/picture/state",
                "screeny/studio/panel/state",
                "screeny/studio/channel/state"
            ]
        );
        for retired in ["scene", "schedule", "resume", "scheduled"] {
            assert!(v["components"][retired]["platform"].is_string(), "the first panel still removes {retired}");
        }
    }

    /// Channel 1 has no device of its own: the first panel's has its picture.
    #[test]
    fn channel_1_is_the_first_devices_and_the_others_get_devices_of_their_own() {
        let out = announce(&config(), &one("x"));
        let configs: Vec<&str> = out.iter().map(|m| m.topic.as_str()).filter(|t| t.ends_with("/config")).collect();
        assert_eq!(configs, ["homeassistant/device/screeny_studio/config", "homeassistant/device/screeny_studio_ch2/config"]);
        let ch2 = out.iter().find(|m| m.topic.ends_with("_ch2/config")).unwrap();
        let v = json(ch2);
        assert_eq!(v["device"]["identifiers"], serde_json::json!(["screeny_studio_ch2"]));
        assert_eq!(v["device"]["name"], "Drawing room");
        let keys: Vec<&str> = v["components"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["patch", "picture"]);
        assert_eq!(v["components"]["picture"]["unique_id"], "screeny_studio_ch2_picture");
        assert_eq!(v["components"]["picture"]["command_topic"], "screeny/studio/ch2/picture/set");
        assert_eq!(v["components"]["picture"]["state_topic"], "screeny/studio/ch2/picture/state");
        assert_eq!(v["components"]["picture"]["availability_topic"], "screeny/studio/status");
        assert_eq!(v["components"]["picture"]["options"].as_array().unwrap().len(), 4);
        assert_eq!(v["components"]["patch"]["unique_id"], "screeny_studio_ch2_patch");
        assert_eq!(v["components"]["patch"]["state_topic"], "screeny/studio/ch2/patch/state");
        // The states follow the config, and one availability for everything.
        let pos = |t: &str| out.iter().position(|m| m.topic == t).unwrap();
        assert!(pos("homeassistant/device/screeny_studio_ch2/config") < pos("screeny/studio/ch2/picture/state"));
        assert_eq!(out.iter().filter(|m| m.topic.ends_with("/status")).count(), 1);
    }

    #[test]
    fn a_later_panel_is_its_own_device_with_a_channel_select_and_no_picture() {
        let cfg = config();
        let panels = vec![view("aa0001", None, "Living room"), view("screeny-4a00a5", Some("screeny-4a00a5"), "Kitchen")];
        let out = announce(&cfg, &fleet(panels));
        let topics: Vec<&str> = out.iter().map(|m| m.topic.as_str()).collect();
        assert_eq!(topics[0], "homeassistant/device/screeny_studio/config");
        assert_eq!(topics[1], "screeny/studio/status");
        assert_eq!(topics[8], "homeassistant/device/screeny_studio_screeny-4a00a5/config");
        assert_eq!(topics[9], "screeny/studio/screeny-4a00a5/brightness/state");
        assert_eq!(topics.iter().filter(|t| t.ends_with("/status")).count(), 1, "one availability for the studio");
        let second = json(&out[8]);
        assert_eq!(second["device"]["identifiers"], serde_json::json!(["screeny_studio_screeny-4a00a5"]));
        assert_eq!(second["device"]["name"], "Kitchen");
        assert_eq!(second["components"]["channel"]["unique_id"], "screeny_studio_screeny-4a00a5_channel");
        assert_eq!(second["components"]["channel"]["command_topic"], "screeny/studio/screeny-4a00a5/channel/set");
        assert_eq!(second["components"]["channel"]["options"], serde_json::json!(["Channel 1", "Drawing room"]));
        assert_eq!(second["components"]["brightness"]["command_topic"], "screeny/studio/screeny-4a00a5/brightness/set");
        // Card 352's picture and patch are removed: listed with their
        // platform and nothing else.
        assert_eq!(second["components"]["picture"], serde_json::json!({ "platform": "select" }));
        assert_eq!(second["components"]["patch"], serde_json::json!({ "platform": "sensor" }));
        assert_eq!(second["components"].as_object().unwrap().len(), 6, "brightness, level, panel, channel, and two removals");
        assert!(!out.iter().any(|m| m.topic == "screeny/studio/screeny-4a00a5/picture/state"), "no state for what it does not have");
        let first = json(&out[0]);
        assert_eq!(first["device"]["name"], "Screeny", "the first is named by the Settings screen, as before");
    }

    #[test]
    fn a_rename_changes_only_that_devices_name() {
        let cfg = config();
        let before = announce(&cfg, &fleet(vec![view("a", None, "One"), view("b", Some("b"), "Kitchen")]));
        let mut f = fleet(vec![view("a", None, "Uno"), view("b", Some("b"), "Pantry")]);
        f.channels[1].name = "Den".into();
        let after = announce(&cfg, &f);
        assert_eq!(before[0], after[0], "the first panel's config does not follow its name");
        let find = |o: &[Message], end: &str| o.iter().find(|m| m.topic.ends_with(end)).cloned().unwrap();
        assert!(find(&after, "_b/config").payload.contains(r#""name":"Pantry""#));
        assert_ne!(find(&before, "_b/config").payload, find(&after, "_b/config").payload);
        assert!(find(&before, "_ch2/config").payload.contains(r#""name":"Drawing room""#));
        assert!(find(&after, "_ch2/config").payload.contains(r#""name":"Den""#), "a channel's device is named after the channel");
    }

    /// A channel rename or delete changes the options of every channel select
    /// - first device and later panels - so their configs are republished.
    #[test]
    fn the_channel_selects_follow_the_channels() {
        let cfg = config();
        let panels = || vec![view("a", None, "One"), view("b", Some("b"), "Kitchen")];
        let before = announce(&cfg, &fleet(panels()));
        let mut renamed = fleet(panels());
        for p in &mut renamed.panels {
            p.snapshot.channels = ChannelOption::all(&[(1, "Channel 1".into()), (2, "Den".into())]);
        }
        let after = announce(&cfg, &renamed);
        for end in ["screeny_studio/config", "_b/config"] {
            let (b, a) = (before.iter().find(|m| m.topic.ends_with(end)).unwrap(), after.iter().find(|m| m.topic.ends_with(end)).unwrap());
            assert_ne!(b.payload, a.payload, "{end}");
            assert!(a.payload.contains("Den") && !a.payload.contains("Drawing room"));
        }
        // Delete: Channel 2 is gone, and its device is no longer announced.
        let mut gone = fleet(panels());
        gone.channels.pop();
        assert!(!announce(&cfg, &gone).iter().any(|m| m.topic.contains("_ch2/")));
        assert_eq!(devices(&cfg, &gone).len(), 2);
        assert_eq!(devices(&cfg, &fleet(panels())).len(), 3);
    }

    #[test]
    fn keys_are_first_none_then_safe_and_unique() {
        let devices: Vec<String> = ["aa0001", "bb0002", "bb 0002", "///", "addr:192.168.7.9"].map(String::from).into();
        assert_eq!(keys(&devices), [None, Some("bb0002".into()), Some("bb_0002".into()), Some("panel".into()), Some("addr_192_168_7_9".into())]);
        let same: Vec<String> = ["a", "x y", "x/y"].map(String::from).into();
        assert_eq!(keys(&same), [None, Some("x_y".into()), Some("x_y_2".into())]);
        assert_eq!(keys(&[]), Vec::<Option<String>>::new());
        // `ch2` is a channel's, never a panel's.
        let clash: Vec<String> = ["a", "ch2", "CH7"].map(String::from).into();
        assert_eq!(keys(&clash), [None, Some("ch2_panel".into()), Some("CH7".into())]);
    }

    #[test]
    fn what_a_forgotten_panel_or_channel_clears() {
        let cfg = config();
        let t = Topics::for_panel(&cfg, Some("b"));
        let topics = retained_topics(&t);
        assert_eq!(topics[0], "homeassistant/device/screeny_studio_b/config");
        assert!(topics[1..].iter().all(|t| t.starts_with("screeny/studio/b/")), "{topics:?}");
        assert_eq!(topics.len(), 7, "the config; brightness, level, panel, channel and card 352's picture and patch");
        assert!(topics.contains(&"screeny/studio/b/picture/state".to_string()) && topics.contains(&"screeny/studio/b/patch/state".to_string()));
        let c = retained_topics(&Topics::for_channel(&cfg, 2));
        assert_eq!(c, ["homeassistant/device/screeny_studio_ch2/config", "screeny/studio/ch2/patch/state", "screeny/studio/ch2/picture/state"]);
    }
}
