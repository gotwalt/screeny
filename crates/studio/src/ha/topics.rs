//! Every topic this integration uses, worked out once from the config.
//!
//! App-owned topics are `screeny/<instance>/<entity>/state` for what the
//! studio says, `.../set` for what HA asks, and `screeny/<instance>/status`
//! for availability. Discovery is one retained config at
//! `<prefix>/device/screeny_<instance>/config`.

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
    /// `screeny/<instance>`.
    pub base: String,
    /// `online` / `offline`, retained; the Last Will says `offline`.
    pub status: String,
    /// The retained device discovery config.
    pub discovery: String,
    /// Where HA says `online` when it (re)starts.
    pub ha_status: String,
    /// `screeny_<instance>`: the device identifier and every `unique_id`'s
    /// stem.
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
        let base = format!("{APP}/{}", cfg.instance);
        let pair = |entity: &str| Pair { state: format!("{base}/{entity}/state"), set: format!("{base}/{entity}/set") };
        let device_id = format!("{APP}_{}", cfg.instance);
        Topics {
            status: format!("{base}/status"),
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

    /// Card 310: the retained state topics of the entities modes and the
    /// timetable took with them. Cleared on every connect, so nothing stale
    /// is left retained.
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
}
