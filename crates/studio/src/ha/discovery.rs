//! The device discovery config, as types.
//!
//! Written against HA's MQTT docs as of 2026.9 (read for card 308, not
//! remembered): device-based discovery, one retained config at
//! `<prefix>/device/<device_id>/config` holding `device`, `origin` (required
//! for device discovery) and a `components` map whose entries each carry
//! `platform` and a `unique_id`. Full key names throughout; HA accepts the
//! abbreviations too but nothing here is short of bytes.
//!
//! Every entity says its own `availability_topic` rather than relying on the
//! device-level shared options, so a reader of one component sees all of it.

use super::topics::Topics;
use super::{MqttConfig, Snapshot};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceDiscovery {
    pub device: Device,
    pub origin: Origin,
    pub components: BTreeMap<String, Component>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Device {
    pub identifiers: Vec<String>,
    pub name: String,
    pub manufacturer: String,
    pub model: String,
    pub sw_version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Origin {
    pub name: String,
    pub sw_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_url: Option<String>,
}

/// One entity. `platform` is the tag HA switches on.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub enum Component {
    Sensor(Sensor),
    Light(Light),
    Select(Select),
    Number(Number),
    BinarySensor(BinarySensor),
}

/// What every entity has.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entity {
    pub unique_id: String,
    /// `None` is serialised as `null`, on purpose: HA then names the entity
    /// after the device alone ("Screeny", not "Screeny Brightness").
    pub name: Option<String>,
    pub availability_topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_category: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Sensor {
    #[serde(flatten)]
    pub entity: Entity,
    pub state_topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_attributes_topic: Option<String>,
}

/// The JSON-schema light: `{"state":"ON","brightness":128}` both ways.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Light {
    #[serde(flatten)]
    pub entity: Entity,
    pub schema: String,
    pub state_topic: String,
    pub command_topic: String,
    pub supported_color_modes: Vec<String>,
    pub brightness_scale: u16,
    /// Neither is something the panel does; saying so keeps HA from offering
    /// them.
    pub flash: bool,
    pub transition: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Select {
    #[serde(flatten)]
    pub entity: Entity,
    pub state_topic: String,
    pub command_topic: String,
    pub options: Vec<String>,
}

/// A slider. HA shows one inline on the device's page, which it does not do
/// for a light's brightness.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Number {
    #[serde(flatten)]
    pub entity: Entity,
    pub state_topic: String,
    pub command_topic: String,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    /// `slider` or `box`.
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_of_measurement: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BinarySensor {
    #[serde(flatten)]
    pub entity: Entity,
    pub state_topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_class: Option<String>,
}

/// The components' keys in the map. Also each `unique_id`'s last part, so
/// they must never change: HA would see new entities and orphan the old.
pub mod key {
    pub const PATCH: &str = "patch";
    pub const BRIGHTNESS: &str = "brightness";
    pub const LEVEL: &str = "level";
    pub const PICTURE: &str = "picture";
    pub const PANEL: &str = "panel";
}

/// Components a previous build announced and this one does not, with their
/// platforms. Card 310 took modes and the timetable out of the studio - Home
/// Assistant picks a picture and keeps the time now - and the Scene select,
/// the schedule switch, button and sensor with them. Device
/// discovery removes a component by listing it with its platform and nothing
/// else; they are listed on every announcement, which costs a few bytes and
/// means a studio that was down when this build first ran still cleans up.
pub const RETIRED: &[(&str, &str)] = &[("scene", "select"), ("schedule", "switch"), ("resume", "button"), ("scheduled", "sensor")];

/// The whole config for this studio as it is now. Only the picture list
/// moves, so this is republished when a named setting is saved, renamed or
/// deleted.
#[must_use]
pub fn build(cfg: &MqttConfig, topics: &Topics, snap: &Snapshot) -> DeviceDiscovery {
    let entity = |key: &str, name: Option<&str>, icon: Option<&str>, category: Option<&str>| Entity {
        unique_id: topics.unique_id(key),
        name: name.map(str::to_string),
        availability_topic: topics.status.clone(),
        icon: icon.map(str::to_string),
        entity_category: category.map(str::to_string),
    };
    let mut components = BTreeMap::new();
    components.insert(
        key::BRIGHTNESS.to_string(),
        Component::Light(Light {
            entity: entity(key::BRIGHTNESS, None, Some("mdi:led-strip"), None),
            schema: "json".into(),
            state_topic: topics.brightness.state.clone(),
            command_topic: topics.brightness.set.clone(),
            supported_color_modes: vec!["brightness".into()],
            brightness_scale: 255,
            flash: false,
            transition: false,
        }),
    );
    components.insert(
        key::LEVEL.to_string(),
        Component::Number(Number {
            entity: entity(key::LEVEL, Some("Brightness"), Some("mdi:brightness-6"), None),
            state_topic: topics.level.state.clone(),
            command_topic: topics.level.set.clone(),
            // The panel has 25 visible steps above dark (card 187), each 4 % of
            // full light: every stop of this slider is a real change, and no
            // two stops show the same picture.
            min: 0.0,
            max: 100.0,
            step: f32::from(super::payload::PERCENT_STEP),
            mode: "slider".into(),
            unit_of_measurement: Some("%".into()),
        }),
    );
    components.insert(
        key::PICTURE.to_string(),
        Component::Select(Select {
            entity: entity(key::PICTURE, Some("Picture"), Some("mdi:palette"), None),
            state_topic: topics.picture.state.clone(),
            command_topic: topics.picture.set.clone(),
            options: snap.pictures.iter().map(|p| p.label.clone()).collect(),
        }),
    );
    components.insert(
        key::PATCH.to_string(),
        Component::Sensor(Sensor {
            entity: entity(key::PATCH, Some("Patch"), Some("mdi:image-filter-hdr"), None),
            state_topic: topics.patch.state.clone(),
            value_template: Some("{{ value_json.name }}".into()),
            json_attributes_topic: Some(topics.patch.state.clone()),
        }),
    );
    components.insert(
        key::PANEL.to_string(),
        Component::BinarySensor(BinarySensor {
            entity: entity(key::PANEL, Some("Panel link"), None, Some("diagnostic")),
            state_topic: topics.panel.state.clone(),
            device_class: Some("connectivity".into()),
        }),
    );
    DeviceDiscovery {
        device: Device {
            identifiers: vec![topics.device_id.clone()],
            name: cfg.name.clone(),
            manufacturer: "screeny".into(),
            model: "Studio".into(),
            sw_version: env!("CARGO_PKG_VERSION").into(),
        },
        origin: Origin { name: "screeny-studio".into(), sw_version: env!("CARGO_PKG_VERSION").into(), support_url: None },
        components,
    }
}

/// What goes on the discovery topic: [`build`], plus the [`RETIRED`]
/// components' removals.
#[must_use]
pub fn payload(cfg: &MqttConfig, topics: &Topics, snap: &Snapshot) -> serde_json::Value {
    let mut value = serde_json::to_value(build(cfg, topics, snap)).unwrap_or_default();
    if let Some(map) = value.get_mut("components").and_then(|c| c.as_object_mut()) {
        for (key, platform) in RETIRED {
            map.insert((*key).to_string(), serde_json::json!({ "platform": platform }));
        }
    }
    value
}

/// Removing the whole device: an empty retained payload on its config topic.
/// HA drops every entity, and the device once nothing else refers to it.
#[must_use]
pub fn remove_device(topics: &Topics) -> (String, Vec<u8>) {
    (topics.discovery.clone(), Vec::new())
}

/// Removing some components and keeping the rest: the same config, with each
/// removed one reduced to `{"platform": "<its platform>"}`.
#[must_use]
pub fn remove_components(mut config: DeviceDiscovery, keys: &[&str]) -> serde_json::Value {
    let mut removed = serde_json::Map::new();
    for key in keys {
        if let Some(c) = config.components.remove(*key) {
            let platform = serde_json::to_value(&c).ok().and_then(|v| v.get("platform").cloned()).unwrap_or_default();
            removed.insert((*key).to_string(), serde_json::json!({ "platform": platform }));
        }
    }
    let mut value = serde_json::to_value(&config).unwrap_or_default();
    if let Some(map) = value.get_mut("components").and_then(|c| c.as_object_mut()) {
        map.extend(removed);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::payload::tests::{config, snapshot_json};

    fn snap() -> Snapshot {
        Snapshot { pictures: crate::ha::payload::tests::pictures(), ..Snapshot::default() }
    }

    #[test]
    fn the_device_config() {
        let cfg = config();
        let topics = Topics::new(&cfg);
        let value = payload(&cfg, &topics, &snap());
        // The version moves with every release; the snapshot says `VERSION`.
        let text = serde_json::to_string_pretty(&value).unwrap().replace(env!("CARGO_PKG_VERSION"), "VERSION");
        snapshot_json("discovery.json", &text);
    }

    #[test]
    fn every_unique_id_is_distinct_and_rooted_in_the_instance() {
        let cfg = config();
        let topics = Topics::new(&cfg);
        let value = serde_json::to_value(build(&cfg, &topics, &snap())).unwrap();
        let ids: Vec<&str> = value["components"].as_object().unwrap().values().filter_map(|c| c["unique_id"].as_str()).collect();
        assert_eq!(ids.len(), 5);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
        assert!(ids.iter().all(|id| id.starts_with("screeny_studio_")));
    }

    #[test]
    fn the_picture_list_is_every_patch_and_setting() {
        let cfg = config();
        let topics = Topics::new(&cfg);
        let d = build(&cfg, &topics, &snap());
        let Component::Select(s) = &d.components[key::PICTURE] else { panic!("the picture is a select") };
        assert_eq!(s.options, ["Overland", "Overland · Dusk", "Vesta", "Vesta · Wall Clock"]);
    }

    #[test]
    fn the_timetables_entities_are_removed() {
        let cfg = config();
        let v = payload(&cfg, &Topics::new(&cfg), &snap());
        assert_eq!(v["components"]["scene"], serde_json::json!({ "platform": "select" }));
        assert_eq!(v["components"]["schedule"], serde_json::json!({ "platform": "switch" }));
        assert_eq!(v["components"]["resume"], serde_json::json!({ "platform": "button" }));
        assert_eq!(v["components"]["scheduled"], serde_json::json!({ "platform": "sensor" }));
    }

    #[test]
    fn removing() {
        let cfg = config();
        let topics = Topics::new(&cfg);
        assert_eq!(remove_device(&topics), ("homeassistant/device/screeny_studio/config".to_string(), Vec::new()));
        let v = remove_components(build(&cfg, &topics, &snap()), &[key::PANEL]);
        assert_eq!(v["components"]["panel"], serde_json::json!({ "platform": "binary_sensor" }));
        assert_eq!(v["components"]["picture"]["platform"], "select", "the others stay whole");
    }
}
