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
//! **The studio does not know about topics** and this module does not know
//! about the studio: [`Snapshot`] goes in, [`Command`] comes out, and
//! [`bridge`] is the one file that holds both ends. Everything between -
//! [`topics`], [`discovery`], [`payload`] - is pure and tested without a
//! broker; [`client`] is the connection and its lifecycle, and nothing else.
//!
//! Brightness is its own control, separate from what is on the panel (owner,
//! 2026-09-26): HA sets it from the room's light sensors, and picks a *scene*
//! (the studio's word is "mode") for what is shown.

pub mod bridge;
pub mod client;
pub mod discovery;
pub mod payload;
pub mod topics;

use serde::Serialize;
use std::fmt;

/// Where the broker is and who this studio is on it. From the environment
/// ([`MqttConfig::from_env`]); `None` there means the integration is off.
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

impl MqttConfig {
    /// `SCREENY_MQTT_HOST` turns the integration on; the rest have defaults.
    ///
    /// | variable | default |
    /// |---|---|
    /// | `SCREENY_MQTT_HOST` | unset: no MQTT at all |
    /// | `SCREENY_MQTT_PORT` | 1883 |
    /// | `SCREENY_MQTT_USER` | none (anonymous) |
    /// | `SCREENY_MQTT_PASSWORD` or `SCREENY_MQTT_PASSWORD_FILE` | none |
    /// | `SCREENY_MQTT_DISCOVERY_PREFIX` | `homeassistant` |
    /// | `SCREENY_MQTT_ID` | `studio` |
    /// | `SCREENY_MQTT_NAME` | `Screeny` |
    ///
    /// `env` is passed in, like `main`'s, so a test need not touch the
    /// process's environment.
    ///
    /// # Errors
    ///
    /// A port that is not a number, a password file that cannot be read, an
    /// id or prefix with nothing usable left after sanitising.
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Result<Option<MqttConfig>, String> {
        let Some(host) = env("SCREENY_MQTT_HOST").map(|h| h.trim().to_string()).filter(|h| !h.is_empty()) else {
            return Ok(None);
        };
        let port = match env("SCREENY_MQTT_PORT") {
            Some(p) => p.trim().parse().map_err(|_| format!("SCREENY_MQTT_PORT {p}: expected a port number"))?,
            None => DEFAULT_PORT,
        };
        let password = match (env("SCREENY_MQTT_PASSWORD"), env("SCREENY_MQTT_PASSWORD_FILE")) {
            (Some(p), _) => Some(p),
            (None, Some(path)) => Some(
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("SCREENY_MQTT_PASSWORD_FILE {path}: {e}"))?
                    .trim_end_matches(['\r', '\n'])
                    .to_string(),
            ),
            (None, None) => None,
        };
        let prefix = env("SCREENY_MQTT_DISCOVERY_PREFIX").unwrap_or_else(|| DEFAULT_PREFIX.to_string());
        let prefix = prefix.trim().trim_matches('/').to_string();
        if prefix.is_empty() || prefix.contains(['+', '#']) {
            return Err(format!("SCREENY_MQTT_DISCOVERY_PREFIX `{prefix}`: expected a topic prefix without wildcards"));
        }
        let raw = env("SCREENY_MQTT_ID").unwrap_or_else(|| DEFAULT_INSTANCE.to_string());
        let instance = topics::sanitize(&raw);
        if instance.is_empty() {
            return Err(format!("SCREENY_MQTT_ID `{raw}`: nothing left after keeping [a-zA-Z0-9_-]"));
        }
        let name = env("SCREENY_MQTT_NAME").map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        Ok(Some(MqttConfig {
            host,
            port,
            username: env("SCREENY_MQTT_USER").filter(|u| !u.is_empty()),
            password,
            discovery_prefix: prefix,
            instance,
            name: name.unwrap_or_else(|| DEFAULT_NAME.to_string()),
        }))
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
    /// Every mode's name, in the order they were made: the scene list.
    pub scenes: Vec<String>,
    /// The scene whose patch and setting are what is playing now, if one is.
    pub scene: Option<String>,
    pub schedule: ScheduleState,
    /// The studio is driving the panel and the link is up.
    pub panel_connected: bool,
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

/// The timetable, as far as HA needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduleState {
    pub enabled: bool,
    /// The scene the timetable says should be on now.
    pub due: Option<String>,
    /// `"HH:MM"`, when the next entry comes due.
    pub until: Option<String>,
    /// Something other than the due scene is playing, by hand.
    pub overridden: bool,
    /// What the last application of a scene had to say, if anything.
    pub note: Option<String>,
}

/// What HA asks for, validated. The only thing that leaves this module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// A brightness policy, `0..=255`; `0` is dark.
    SetBrightness(u8),
    /// Put the panel into the scene called this. Already checked against
    /// [`Snapshot::scenes`]; the studio checks again, since it may have gone.
    ApplyScene(String),
    /// Switch the timetable on or off.
    SetSchedule(bool),
    /// "Back to schedule": apply the entry due now.
    ResumeSchedule,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn no_host_means_no_mqtt() {
        assert_eq!(MqttConfig::from_env(&env(&[])), Ok(None));
        assert_eq!(MqttConfig::from_env(&env(&[("SCREENY_MQTT_HOST", "  ")])), Ok(None));
    }

    #[test]
    fn defaults() {
        let cfg = MqttConfig::from_env(&env(&[("SCREENY_MQTT_HOST", "broker.example")])).unwrap().unwrap();
        assert_eq!(cfg.port, 1883);
        assert_eq!(cfg.discovery_prefix, "homeassistant");
        assert_eq!(cfg.instance, "studio");
        assert_eq!(cfg.name, "Screeny");
        assert_eq!(cfg.username, None);
        assert_eq!(cfg.password, None);
    }

    #[test]
    fn everything_set() {
        let cfg = MqttConfig::from_env(&env(&[
            ("SCREENY_MQTT_HOST", "broker.example"),
            ("SCREENY_MQTT_PORT", "1884"),
            ("SCREENY_MQTT_USER", "someone"),
            ("SCREENY_MQTT_PASSWORD", "password9"),
            ("SCREENY_MQTT_DISCOVERY_PREFIX", "/ha/"),
            ("SCREENY_MQTT_ID", "Living Room!"),
            ("SCREENY_MQTT_NAME", "Living room panel"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(cfg.port, 1884);
        assert_eq!(cfg.discovery_prefix, "ha");
        assert_eq!(cfg.instance, "Living_Room", "sanitised for topics");
        assert_eq!(cfg.name, "Living room panel");
        assert_eq!(cfg.password.as_deref(), Some("password9"));
    }

    #[test]
    fn a_password_file_loses_its_newline() {
        let path = std::env::temp_dir().join(format!("screeny-mqtt-pw-{}", std::process::id()));
        std::fs::write(&path, "password9\n").unwrap();
        let p = path.to_string_lossy().to_string();
        let cfg = MqttConfig::from_env(&env(&[("SCREENY_MQTT_HOST", "b"), ("SCREENY_MQTT_PASSWORD_FILE", &p)])).unwrap().unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(cfg.password.as_deref(), Some("password9"));
    }

    #[test]
    fn bad_values_are_refused() {
        assert!(MqttConfig::from_env(&env(&[("SCREENY_MQTT_HOST", "b"), ("SCREENY_MQTT_PORT", "x")])).is_err());
        assert!(MqttConfig::from_env(&env(&[("SCREENY_MQTT_HOST", "b"), ("SCREENY_MQTT_ID", "///")])).is_err());
        assert!(MqttConfig::from_env(&env(&[("SCREENY_MQTT_HOST", "b"), ("SCREENY_MQTT_DISCOVERY_PREFIX", "a/#")])).is_err());
    }

    #[test]
    fn debug_hides_the_password() {
        let cfg = MqttConfig::from_env(&env(&[("SCREENY_MQTT_HOST", "b"), ("SCREENY_MQTT_PASSWORD", "password9")])).unwrap().unwrap();
        let shown = format!("{cfg:?}");
        assert!(!shown.contains("password9"), "{shown}");
        assert!(shown.contains("<set>"));
    }
}
