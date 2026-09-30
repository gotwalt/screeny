//! Card 308: Home Assistant, over MQTT discovery.
//!
//! The studio announces itself to Home Assistant's own MQTT integration - one
//! retained device config with a `components` map - and HA makes the entities.
//! No custom component on the HA side, no YAML.
//!
//! ```text
//!   AppState ──bridge──> watch<Snapshot> ──> client task ──retained state──> broker ──> HA
//!      ^                                        │
//!      └──────bridge<── mpsc<Command> <── parse `.../set`  <──────────────── broker <── HA
//! ```
//!
//! **One HA device per panel (card 352), one per channel (card 355).** A
//! [`Fleet`] - a [`PanelView`] per panel and a [`ChannelView`] per channel -
//! goes in and an [`Order`] (a [`Command`] and what it is for) comes out. The
//! first panel keeps the topics, discovery id and `unique_id`s every earlier
//! build had, and its picture and patch are now **Channel 1's**; later panels
//! are keyed by their device id ([`topics::Topics::for_panel`]) and have a
//! channel select in place of a picture; every other channel is a device of
//! its own, `screeny_<instance>_ch<id>`, with the picture and the patch
//! ([`topics::Topics::for_channel`]). The channel owns the picture, so HA
//! picks it there, and a panel's channel select moves the panel.
//!
//! **The studio does not know about topics** and this module does not know
//! about the studio: [`Snapshot`] goes in, [`Command`] comes out, and
//! [`bridge`] is the one file that holds both ends. Everything between -
//! [`topics`], [`discovery`], [`payload`] - is pure and tested without a
//! broker; [`client`] is the connection and its lifecycle, and nothing else.
//!
//! Brightness is its own control, separate from what is on the panel (owner,
//! 2026-09-26): HA sets it from the room's light sensors. What is shown is a
//! **picture**: a patch on one of its named settings, picked from one list.
//! **Time of day is HA's business entirely** (card 310): the studio has no
//! modes and no timetable, so a schedule is an HA automation that picks a
//! picture.

pub mod bridge;
pub mod client;
pub mod discovery;
pub mod fleet;
pub mod payload;
pub mod topics;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Mutex;
use tokio::sync::watch;

/// Where the broker is and who this studio is on it: one connection's worth
/// of [`HaSettings`], made only when those say to connect.
#[derive(Clone, PartialEq, Eq)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    /// HA's discovery prefix. `homeassistant` unless HA was told otherwise.
    pub discovery_prefix: String,
    /// The stable id every topic and `unique_id` is built from. Already
    /// sanitised to `[a-zA-Z0-9_-]`. Change it and HA sees a new device.
    pub instance: String,
    /// The device's name in HA, which HA also puts in front of every entity's.
    pub name: String,
}

/// The password stays out of every `{:?}`, which is where a config ends up
/// when something goes wrong.
impl fmt::Debug for MqttConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MqttConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<set>"))
            .field("discovery_prefix", &self.discovery_prefix)
            .field("instance", &self.instance)
            .field("name", &self.name)
            .finish()
    }
}

pub const DEFAULT_PORT: u16 = 1883;
pub const DEFAULT_PREFIX: &str = "homeassistant";
pub const DEFAULT_INSTANCE: &str = "studio";
pub const DEFAULT_NAME: &str = "Screeny";

/// Card 311: the integration as the owner set it up on the Settings screen,
/// kept in `state.json`. **Written from the page, never from the
/// environment**: there is one place to change it.
///
/// The password is in the state file in the clear - it is on the studio's own
/// volume, beside everything else the studio keeps - and it **never goes back
/// out**: the API says only whether one is set ([`HaView`]).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HaSettings {
    /// Connect at all.
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    /// Empty: anonymous.
    pub username: String,
    pub password: String,
    pub discovery_prefix: String,
    pub instance: String,
    pub name: String,
}

impl Default for HaSettings {
    fn default() -> Self {
        HaSettings {
            enabled: false,
            host: String::new(),
            port: DEFAULT_PORT,
            username: String::new(),
            password: String::new(),
            discovery_prefix: DEFAULT_PREFIX.to_string(),
            instance: DEFAULT_INSTANCE.to_string(),
            name: DEFAULT_NAME.to_string(),
        }
    }
}

/// As [`MqttConfig`]'s: the password stays out of every `{:?}`.
impl fmt::Debug for HaSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HaSettings")
            .field("enabled", &self.enabled)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &(!self.password.is_empty()).then_some("<set>"))
            .field("discovery_prefix", &self.discovery_prefix)
            .field("instance", &self.instance)
            .field("name", &self.name)
            .finish()
    }
}

impl HaSettings {
    /// Tidy, and refuse what cannot work, in a sentence the page can show.
    ///
    /// # Errors
    ///
    /// Switched on with no broker; a port of 0; an id with nothing usable in
    /// it; a discovery prefix with a wildcard in it.
    pub fn check(mut self) -> Result<HaSettings, String> {
        self.host = self.host.trim().to_string();
        self.username = self.username.trim().to_string();
        self.name = self.name.trim().to_string();
        if self.name.is_empty() {
            DEFAULT_NAME.clone_into(&mut self.name);
        }
        let prefix = self.discovery_prefix.trim().trim_matches('/').to_string();
        if prefix.is_empty() || prefix.contains(['+', '#']) {
            return Err(format!("`{prefix}` cannot be a discovery prefix; Home Assistant's own is `{DEFAULT_PREFIX}`."));
        }
        self.discovery_prefix = prefix;
        let raw = self.instance.trim().to_string();
        let instance = if raw.is_empty() { DEFAULT_INSTANCE.to_string() } else { topics::sanitize(&raw) };
        if instance.is_empty() {
            return Err(format!("`{raw}` has nothing in it an id can use: letters, digits, `_` and `-`."));
        }
        self.instance = instance;
        if self.port == 0 {
            return Err("The broker's port cannot be 0; Home Assistant's is usually 1883.".into());
        }
        if self.enabled && self.host.is_empty() {
            return Err("Say where the MQTT broker is before switching this on.".into());
        }
        Ok(self)
    }

    /// The connection these describe, or `None` for "do not connect".
    #[must_use]
    pub fn connection(&self) -> Option<MqttConfig> {
        (self.enabled && !self.host.is_empty()).then(|| MqttConfig {
            host: self.host.clone(),
            port: self.port,
            username: (!self.username.is_empty()).then(|| self.username.clone()),
            password: (!self.password.is_empty()).then(|| self.password.clone()),
            discovery_prefix: self.discovery_prefix.clone(),
            instance: self.instance.clone(),
            name: self.name.clone(),
        })
    }

    /// Settings that connect as `cfg` does - how a test, or a library caller,
    /// starts a studio already connected ([`crate::Config::mqtt`]).
    #[must_use]
    pub fn connecting_as(cfg: &MqttConfig) -> HaSettings {
        HaSettings {
            enabled: true,
            host: cfg.host.clone(),
            port: cfg.port,
            username: cfg.username.clone().unwrap_or_default(),
            password: cfg.password.clone().unwrap_or_default(),
            discovery_prefix: cfg.discovery_prefix.clone(),
            instance: cfg.instance.clone(),
            name: cfg.name.clone(),
        }
    }
}

/// How the connection is doing, for the Settings screen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Switched off, or not set up.
    Off,
    Connecting,
    Connected,
    /// Trying again, with the last reason it did not work.
    Failed { detail: String },
}

/// What the page is told: the settings without the password, how the
/// connection is doing, and where to look on the broker.
#[derive(Clone, Debug, Serialize)]
pub struct HaView {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Whether a password is saved. The password itself never leaves.
    pub password_set: bool,
    pub discovery_prefix: String,
    pub instance: String,
    pub name: String,
    pub status: Status,
    /// `homeassistant/device/screeny_<id>/config`.
    pub discovery_topic: String,
    /// `screeny/<id>`.
    pub topic_base: String,
}

/// The integration's settings and its state, shared by the page's routes and
/// the task that keeps the connection ([`client::supervise`]).
#[derive(Debug)]
pub struct Ha {
    settings: Mutex<HaSettings>,
    /// The connection to keep: `None` for none. The supervisor restarts the
    /// client whenever this changes.
    pub(crate) want: watch::Sender<Option<MqttConfig>>,
    pub(crate) status: watch::Sender<Status>,
}

impl Ha {
    #[must_use]
    pub fn new(settings: HaSettings) -> Ha {
        let want = watch::Sender::new(settings.connection());
        Ha { settings: Mutex::new(settings), want, status: watch::Sender::new(Status::Off) }
    }

    #[must_use]
    pub fn settings(&self) -> HaSettings {
        self.settings.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Take new settings - already [`HaSettings::check`]ed - and reconnect if
    /// the connection they describe is a different one.
    pub fn set(&self, settings: HaSettings) {
        let conn = settings.connection();
        *self.settings.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
        self.want.send_if_modified(|was| {
            if *was == conn {
                return false;
            }
            *was = conn;
            true
        });
    }

    #[must_use]
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// Wait - at most `within` - for the connection to be off.
    pub async fn until_off(&self, within: std::time::Duration) {
        let mut rx = self.status.subscribe();
        let _ = tokio::time::timeout(within, rx.wait_for(|s| *s == Status::Off)).await;
    }

    #[must_use]
    pub fn view(&self) -> HaView {
        let s = self.settings();
        let t = topics::Topics::new(&MqttConfig {
            host: String::new(),
            port: s.port,
            username: None,
            password: None,
            discovery_prefix: s.discovery_prefix.clone(),
            instance: s.instance.clone(),
            name: s.name.clone(),
        });
        HaView {
            enabled: s.enabled,
            host: s.host,
            port: s.port,
            username: s.username,
            password_set: !s.password.is_empty(),
            discovery_prefix: s.discovery_prefix,
            instance: s.instance,
            name: s.name,
            status: self.status(),
            discovery_topic: t.discovery,
            topic_base: t.base,
        }
    }
}

/// Everything HA is shown, as plain values. The studio builds one of these
/// ([`bridge`]) and this module turns it into retained messages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub patch: PatchState,
    /// The panel's brightness policy, `0..=255`, `0` dark. `None` when the
    /// studio has never been told one and the panel is at its own.
    pub brightness: Option<u8>,
    /// Every picture HA may pick: each patch this studio can play, on
    /// Default and on each of its named settings.
    pub pictures: Vec<Picture>,
    /// The label of the picture playing now; `None` when the working copy
    /// has been changed since its setting was loaded, which is no picture in
    /// the list.
    pub picture: Option<String>,
    /// The studio is driving the panel and the link is up.
    pub panel_connected: bool,
    /// Card 355: every channel, as a select's options, Channel 1 first.
    pub channels: Vec<ChannelOption>,
    /// The id of the channel this panel is on; `None` for the stand-in that
    /// stands for no panel.
    pub channel: Option<u32>,
}

/// One entry in HA's channel select.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelOption {
    pub id: u32,
    /// What HA shows: the channel's name, and `Name (<id>)` when two
    /// channels have been given the same name, so the choice stays a choice.
    pub label: String,
}

impl ChannelOption {
    /// The options for channels `(id, name)`, labels made unique.
    #[must_use]
    pub fn all(channels: &[(u32, String)]) -> Vec<ChannelOption> {
        channels
            .iter()
            .map(|(id, name)| {
                let twice = channels.iter().filter(|(_, n)| n == name).count() > 1;
                ChannelOption { id: *id, label: if twice { format!("{name} ({id})") } else { name.clone() } }
            })
            .collect()
    }
}

/// What is on the panel. Published as it is, as JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PatchState {
    /// The patch's id, e.g. `overland`.
    pub id: String,
    /// Its name as people read it, e.g. `Overland`.
    pub name: String,
    /// The named setting it was loaded from; `Default` for the patch's own.
    pub setting: String,
    /// The setting has been changed since it was loaded.
    pub modified: bool,
}

/// One entry in HA's picture list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// What HA shows: `Vesta` for a patch on Default, `Vesta · Wall Clock` for
    /// one of its named settings (owner, 2026-09-26).
    pub label: String,
    /// The patch's id.
    pub patch: String,
    /// The setting's name; `Default` for the patch's own.
    pub setting: String,
}

impl Picture {
    /// The label for `patch_name` on `setting`.
    #[must_use]
    pub fn label(patch_name: &str, setting: &str) -> String {
        if crate::state::is_default_name(setting) {
            patch_name.to_string()
        } else {
            format!("{patch_name} · {setting}")
        }
    }
}

/// One panel's HA device: who it is, what to call it, and what it shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelView {
    /// The studio's device id: what an [`Order`] is addressed to. Empty for
    /// the stand-in that stands for no panel.
    pub device: String,
    /// `None` for the first panel, whose topics and ids are the ones every
    /// earlier build had; otherwise the sanitised, unique key that goes in
    /// its topics and ids.
    pub key: Option<String>,
    /// The HA device's name.
    pub name: String,
    /// The panel's own brightness and link, its channel and the channel
    /// options; for the first panel, **Channel 1's** picture too.
    pub snapshot: Snapshot,
}

/// One channel: what HA needs to give it a device of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelView {
    pub id: u32,
    /// The HA device's name: the channel's.
    pub name: String,
    /// Its picture: the patch, the pictures HA may pick and the one playing.
    pub snapshot: Snapshot,
}

/// Everything HA is shown (cards 352, 355): every panel's device, first panel
/// first, and every channel. Channel 1 has no device of its own; it is the
/// first panel's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fleet {
    pub panels: Vec<PanelView>,
    /// Every channel, Channel 1 first.
    pub channels: Vec<ChannelView>,
}

/// What a command is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A panel, by device id; empty is the first panel.
    Panel(String),
    /// A channel, by id.
    Channel(u32),
}

/// A command, and what it is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub target: Target,
    pub command: Command,
}

/// What HA asks for, validated. The only thing that leaves this module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// A brightness policy, `0..=255`; `0` is dark.
    SetBrightness(u8),
    /// Show this patch on this named setting (`Default` for its own). Already
    /// checked against [`Snapshot::pictures`]; the studio checks again, since
    /// a setting may have gone.
    ShowPicture { patch: String, setting: String },
    /// Card 355: move the panel to this channel. Checked against
    /// [`Snapshot::channels`]; the studio checks again, since a channel may
    /// have gone.
    MoveToChannel(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(host: &str) -> HaSettings {
        HaSettings { enabled: true, host: host.into(), ..HaSettings::default() }
    }

    #[test]
    fn defaults_are_home_assistants_and_off() {
        let s = HaSettings::default();
        assert!(!s.enabled);
        assert_eq!((s.port, s.discovery_prefix.as_str(), s.instance.as_str(), s.name.as_str()), (1883, "homeassistant", "studio", "Screeny"));
        assert_eq!(s.check().unwrap().connection(), None, "off connects nowhere");
    }

    #[test]
    fn check_tidies_and_refuses() {
        let s = HaSettings {
            host: " broker.example ".into(),
            username: " someone ".into(),
            discovery_prefix: "/ha/".into(),
            instance: "Living Room!".into(),
            name: "  ".into(),
            ..on("x")
        }
        .check()
        .unwrap();
        assert_eq!((s.host.as_str(), s.username.as_str()), ("broker.example", "someone"));
        assert_eq!((s.discovery_prefix.as_str(), s.instance.as_str(), s.name.as_str()), ("ha", "Living_Room", "Screeny"));

        for bad in [
            on(""),
            HaSettings { port: 0, ..on("b") },
            HaSettings { instance: "///".into(), ..on("b") },
            HaSettings { discovery_prefix: "a/#".into(), ..on("b") },
        ] {
            let e = bad.check().unwrap_err();
            assert!(e.ends_with('.'), "a sentence: {e}");
        }
        assert!(HaSettings { host: String::new(), ..HaSettings::default() }.check().is_ok(), "off may be empty");
    }

    #[test]
    fn a_connection_only_when_on_and_somewhere() {
        let c = HaSettings { username: "someone".into(), password: "password9".into(), ..on("b") }.connection().unwrap();
        assert_eq!((c.host.as_str(), c.username.as_deref(), c.password.as_deref()), ("b", Some("someone"), Some("password9")));
        assert_eq!(on("b").connection().unwrap().username, None, "empty is anonymous");
        assert_eq!(HaSettings { enabled: false, ..on("b") }.connection(), None);
    }

    #[test]
    fn the_password_never_leaves() {
        let ha = Ha::new(HaSettings { password: "password9".into(), ..on("b") });
        let view = serde_json::to_string(&ha.view()).unwrap();
        assert!(!view.contains("password9"), "{view}");
        assert!(view.contains(r#""password_set":true"#), "{view}");
        assert!(!format!("{:?}", ha.settings()).contains("password9"));
        let c = ha.settings().connection().unwrap();
        assert!(!format!("{c:?}").contains("password9"));
    }

    #[test]
    fn set_reconnects_only_when_the_connection_changes() {
        let ha = Ha::new(on("b"));
        let mut want = ha.want.subscribe();
        ha.set(HaSettings { name: "Screeny".into(), ..on("b") });
        assert!(!want.has_changed().unwrap(), "the same connection");
        ha.set(on("c"));
        assert!(want.has_changed().unwrap());
        assert_eq!(want.borrow_and_update().as_ref().map(|c| c.host.clone()).as_deref(), Some("c"));
        ha.set(HaSettings { enabled: false, ..on("c") });
        assert_eq!(*want.borrow_and_update(), None);
    }

    #[test]
    fn a_file_with_only_some_keys_reads_the_rest_as_defaults() {
        let s: HaSettings = serde_json::from_str(r#"{"enabled":true,"host":"b"}"#).unwrap();
        assert_eq!(s, on("b"));
    }
}
