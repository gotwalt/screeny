//! Every topic this integration uses, worked out once from the config.
//!
//! App-owned topics are `screeny/<instance>/<entity>/state` for what the
//! studio says, `.../set` for what HA asks, and `screeny/<instance>/status`
//! for availability. Discovery is one retained config at
//! `<prefix>/device/screeny_<instance>/config`.
//!
//! **Card 352: one HA device per panel.** The *first* panel (adoption order)
//! has exactly the topics above and ids, so the owner's automations keep
//! working. A later panel keyed `<key>` (its sanitised device id) has entity
//! topics under `screeny/<instance>/<key>/<entity>/...`, discovery id
//! `screeny_<instance>_<key>`, and `unique_id`s rooted in that. Availability
//! stays one topic for the whole studio, `screeny/<instance>/status`, because
//! an MQTT connection has one Last Will.

use super::MqttConfig;

/// The root of every topic the studio owns.
pub const APP: &str = "screeny";

/// Keep `[a-zA-Z0-9_-]`, turn anything else into `_`, and trim `_` and `-`
/// off the ends. MQTT would take more, but HA builds ids and entity ids from
/// these and a `/`, `+` or `#` in a topic level is a different topic.
#[must_use]
pub fn sanitize(raw: &str) -> String {
    let kept: String = raw.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    kept.trim_matches(['_', '-']).to_string()
}

/// One entity's pair of topics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub state: String,
    pub set: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topics {
    /// `None` for the first panel, `Some(<key>)` for a later one.
    pub key: Option<String>,
    /// `screeny/<instance>` (first panel), `screeny/<instance>/<key>` (later).
    pub base: String,
    /// `screeny/<instance>`, whatever the panel: the studio's own root.
    pub root: String,
    /// `online` / `offline`, retained; the Last Will says `offline`.
    pub status: String,
    /// The retained device discovery config.
    pub discovery: String,
    /// Where HA says `online` when it (re)starts.
    pub ha_status: String,
    /// `screeny_<instance>` (first panel) or `screeny_<instance>_<key>`: the
    /// device identifier and every `unique_id`'s stem.
    pub device_id: String,
    pub patch: Pair,
    pub brightness: Pair,
    /// Card 310: the same brightness as a 0-100 % slider, which HA shows on the
    /// device's own page (a light there is only a switch).
    pub level: Pair,
    pub picture: Pair,
    pub panel: Pair,
}

impl Topics {
    #[must_use]
    pub fn new(cfg: &MqttConfig) -> Topics {
        Topics::for_panel(cfg, None)
    }

    /// The topics of one panel's HA device: `None` is the first panel, which
    /// keeps the layout every earlier build had.
    #[must_use]
    pub fn for_panel(cfg: &MqttConfig, key: Option<&str>) -> Topics {
        let root = format!("{APP}/{}", cfg.instance);
        let base = key.map_or_else(|| root.clone(), |k| format!("{root}/{k}"));
        let pair = |entity: &str| Pair { state: format!("{base}/{entity}/state"), set: format!("{base}/{entity}/set") };
        let device_id = key.map_or_else(|| format!("{APP}_{}", cfg.instance), |k| format!("{APP}_{}_{k}", cfg.instance));
        Topics {
            key: key.map(str::to_string),
            status: format!("{root}/status"),
            root,
            discovery: format!("{}/device/{device_id}/config", cfg.discovery_prefix),
            ha_status: format!("{}/status", cfg.discovery_prefix),
            patch: pair("patch"),
            brightness: pair("brightness"),
            level: pair("level"),
            picture: pair("picture"),
            panel: pair("panel"),
            device_id,
            base,
        }
    }

    /// Every topic HA may send a command on. Subscribed on every connect.
    #[must_use]
    pub fn command_topics(&self) -> [&str; 3] {
        [&self.brightness.set, &self.level.set, &self.picture.set]
    }

    /// What to subscribe to so every panel's commands arrive, however many
    /// there are and whenever they were adopted: `.../<entity>/set` for the
    /// first panel and `.../<key>/<entity>/set` for the rest. A message on a
    /// topic no known panel owns is dropped by the session.
    #[must_use]
    pub fn command_wildcards(&self) -> [String; 2] {
        [format!("{}/+/set", self.root), format!("{}/+/+/set", self.root)]
    }

    /// Does this panel's HA device carry the [`super::discovery::RETIRED`]
    /// removals? Only the first, which is the only one a previous build
    /// announced.
    #[must_use]
    pub fn is_first(&self) -> bool {
        self.key.is_none()
    }

    /// Card 310: the retained state topics of the entities modes and the
    /// timetable took with them. Cleared on every connect, so nothing stale
    /// is left retained. Only the first panel ever had them.
    #[must_use]
    pub fn retired_state_topics(&self) -> [String; 3] {
        ["schedule", "scheduled", "scene"].map(|e| format!("{}/{e}/state", self.base))
    }

    /// `screeny_<instance>_<entity>`.
    #[must_use]
    pub fn unique_id(&self, entity: &str) -> String {
        format!("{}_{entity}", self.device_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_only_what_ha_accepts() {
        assert_eq!(sanitize("studio"), "studio");
        assert_eq!(sanitize("living-room_2"), "living-room_2");
        assert_eq!(sanitize(" Living Room! "), "Living_Room");
        assert_eq!(sanitize("a/b+c#d"), "a_b_c_d");
        assert_eq!(sanitize("émile"), "mile");
        assert_eq!(sanitize("///"), "");
    }

    #[test]
    fn the_layout() {
        let cfg = MqttConfig {
            host: "b".into(),
            port: 1883,
            username: None,
            password: None,
            discovery_prefix: "homeassistant".into(),
            instance: "studio".into(),
            name: "Screeny".into(),
        };
        let t = Topics::new(&cfg);
        assert_eq!(t.status, "screeny/studio/status");
        assert_eq!(t.discovery, "homeassistant/device/screeny_studio/config");
        assert_eq!(t.ha_status, "homeassistant/status");
        assert_eq!(t.brightness.state, "screeny/studio/brightness/state");
        assert_eq!(t.brightness.set, "screeny/studio/brightness/set");
        assert_eq!(t.picture.set, "screeny/studio/picture/set");
        assert_eq!(t.level.set, "screeny/studio/level/set");
        assert_eq!(t.unique_id("scene"), "screeny_studio_scene");
    }

    #[test]
    fn a_later_panel_has_its_own_device_and_shares_the_availability() {
        let cfg = MqttConfig {
            host: "b".into(),
            port: 1883,
            username: None,
            password: None,
            discovery_prefix: "homeassistant".into(),
            instance: "studio".into(),
            name: "Screeny".into(),
        };
        let t = Topics::for_panel(&cfg, Some("screeny-4a00a5"));
        assert_eq!(t.base, "screeny/studio/screeny-4a00a5");
        assert_eq!(t.status, "screeny/studio/status", "one Last Will for the studio");
        assert_eq!(t.discovery, "homeassistant/device/screeny_studio_screeny-4a00a5/config");
        assert_eq!(t.picture.set, "screeny/studio/screeny-4a00a5/picture/set");
        assert_eq!(t.unique_id("panel"), "screeny_studio_screeny-4a00a5_panel");
        assert_eq!(t.command_wildcards(), ["screeny/studio/+/set", "screeny/studio/+/+/set"]);
        assert!(!t.is_first() && Topics::new(&cfg).is_first());
    }
}
