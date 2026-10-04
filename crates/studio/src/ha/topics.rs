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

/// Whose topics these are (card 355).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The first panel's device: the original ids, plus **Channel 1's**
    /// picture and patch.
    First,
    /// A later panel's device: light, brightness, link and a channel select.
    Panel,
    /// A channel other than Channel 1: a picture select and a patch sensor.
    Channel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topics {
    pub role: Role,
    /// `None` for the first panel, `Some(<key>)` for a later one, and
    /// `Some("ch<id>")` for a channel.
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
    /// Card 355: which channel the panel is on, as a select of channel names.
    pub channel: Pair,
    /// Card 364: the panel's firmware, an `update` entity. A panel's only.
    pub firmware: Pair,
}

/// The key a channel's device and topics carry: `ch<id>`.
#[must_use]
pub fn channel_key(id: u32) -> String {
    format!("ch{id}")
}

/// Is this a key [`channel_key`] could make? A panel's key may not be one.
#[must_use]
pub fn is_channel_key(key: &str) -> bool {
    key.strip_prefix("ch").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
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
        Topics::with_role(cfg, key, if key.is_none() { Role::First } else { Role::Panel })
    }

    /// The topics of the HA device of a channel other than Channel 1:
    /// `screeny/<instance>/ch<id>/...`, discovery id `screeny_<instance>_ch<id>`.
    #[must_use]
    pub fn for_channel(cfg: &MqttConfig, id: u32) -> Topics {
        Topics::with_role(cfg, Some(&channel_key(id)), Role::Channel)
    }

    fn with_role(cfg: &MqttConfig, key: Option<&str>, role: Role) -> Topics {
        let root = format!("{APP}/{}", cfg.instance);
        let base = key.map_or_else(|| root.clone(), |k| format!("{root}/{k}"));
        let pair = |entity: &str| Pair { state: format!("{base}/{entity}/state"), set: format!("{base}/{entity}/set") };
        let device_id = key.map_or_else(|| format!("{APP}_{}", cfg.instance), |k| format!("{APP}_{}_{k}", cfg.instance));
        Topics {
            role,
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
            channel: pair("channel"),
            firmware: pair("firmware"),
            device_id,
            base,
        }
    }

    /// Every topic HA may send a command on to this device: a channel takes a
    /// picture; a panel a brightness and a channel; the first panel, which
    /// also carries Channel 1, all of it.
    #[must_use]
    pub fn command_topics(&self) -> Vec<&str> {
        match self.role {
            Role::First => vec![&self.brightness.set, &self.level.set, &self.picture.set, &self.channel.set, &self.firmware.set],
            Role::Panel => vec![&self.brightness.set, &self.level.set, &self.channel.set, &self.firmware.set],
            Role::Channel => vec![&self.picture.set],
        }
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
        self.role == Role::First
    }

    /// The retained state topics of entities this device no longer has, to be
    /// cleared on every connect so nothing stale is left on the broker. The
    /// first panel's are card 310's (modes and the timetable); a later
    /// panel's are card 355's (its picture and patch became its channel's).
    #[must_use]
    pub fn retired_state_topics(&self) -> Vec<String> {
        match self.role {
            Role::First => ["schedule", "scheduled", "scene"].iter().map(|e| format!("{}/{e}/state", self.base)).collect(),
            Role::Panel => vec![self.picture.state.clone(), self.patch.state.clone()],
            Role::Channel => Vec::new(),
        }
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
        assert_eq!(t.channel.set, "screeny/studio/screeny-4a00a5/channel/set");
        assert_eq!(t.command_topics().len(), 4, "brightness, level, a channel and firmware: no picture");
        assert_eq!(t.firmware.set, "screeny/studio/screeny-4a00a5/firmware/set");
    }

    #[test]
    fn a_channel_has_its_own_device_with_a_picture_and_nothing_else_to_command() {
        let cfg = MqttConfig {
            host: "b".into(),
            port: 1883,
            username: None,
            password: None,
            discovery_prefix: "homeassistant".into(),
            instance: "studio".into(),
            name: "Screeny".into(),
        };
        let t = Topics::for_channel(&cfg, 2);
        assert_eq!(t.discovery, "homeassistant/device/screeny_studio_ch2/config");
        assert_eq!(t.device_id, "screeny_studio_ch2");
        assert_eq!(t.picture.set, "screeny/studio/ch2/picture/set");
        assert_eq!(t.patch.state, "screeny/studio/ch2/patch/state");
        assert_eq!(t.command_topics(), ["screeny/studio/ch2/picture/set"]);
        assert!(is_channel_key("ch12") && !is_channel_key("ch") && !is_channel_key("chx2") && !is_channel_key("ch2a"));
        // Channel 1's picture is where it always was, on the first panel.
        let first = Topics::new(&cfg);
        assert_eq!(first.picture.set, "screeny/studio/picture/set");
        assert_eq!(first.channel.set, "screeny/studio/channel/set");
        assert_eq!(first.command_topics().len(), 5);
        assert_eq!(first.firmware.state, "screeny/studio/firmware/state");
    }
}
