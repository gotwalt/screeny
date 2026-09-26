//! What goes on the state topics, and what comes off the command topics.
//!
//! Pure: a [`Snapshot`] in, messages out; a topic and bytes in, a validated
//! [`Command`] or a sentence saying why not.

use super::topics::Topics;
use super::{Command, Snapshot};
use serde::{Deserialize, Serialize};

/// A command payload longer than this is not one of ours.
pub const MAX_COMMAND_BYTES: usize = 1024;
/// What "on" means when HA says only "on" and the panel has never been lit
/// in this process: the middle of the scale, never full.
pub const DEFAULT_LIT: u8 = 128;
/// A select's state for "none of the options" - HA shows it as unknown.
pub const NO_OPTION: &str = "None";

/// One retained state message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub topic: String,
    pub payload: String,
}

/// The JSON light's state.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct LightState {
    /// `null` when the brightness is not known.
    state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brightness: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color_mode: Option<&'static str>,
}

/// The schedule sensor's state and attributes.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct ScheduledState<'a> {
    scene: Option<&'a str>,
    until: Option<&'a str>,
    overridden: bool,
    note: Option<&'a str>,
}

/// These types have no maps with non-string keys and nothing that can fail
/// to serialise.
fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn on_off(on: bool) -> String {
    if on { "ON" } else { "OFF" }.to_string()
}

/// Every state topic's payload for `snap`. All retained, so HA has the right
/// values after its own restart.
#[must_use]
pub fn state_messages(topics: &Topics, snap: &Snapshot) -> Vec<Message> {
    let light = match snap.brightness {
        None => LightState { state: None, brightness: None, color_mode: None },
        Some(0) => LightState { state: Some("OFF"), brightness: None, color_mode: None },
        Some(b) => LightState { state: Some("ON"), brightness: Some(b), color_mode: Some("brightness") },
    };
    let scheduled = ScheduledState {
        scene: snap.schedule.due.as_deref(),
        until: snap.schedule.until.as_deref(),
        overridden: snap.schedule.overridden,
        note: snap.schedule.note.as_deref(),
    };
    vec![
        Message { topic: topics.patch.state.clone(), payload: json(&snap.patch) },
        Message { topic: topics.brightness.state.clone(), payload: json(&light) },
        Message { topic: topics.scene.state.clone(), payload: snap.scene.clone().unwrap_or_else(|| NO_OPTION.to_string()) },
        Message { topic: topics.schedule.state.clone(), payload: on_off(snap.schedule.enabled) },
        Message { topic: topics.scheduled.state.clone(), payload: json(&scheduled) },
        Message { topic: topics.panel.state.clone(), payload: on_off(snap.panel_connected) },
    ]
}

/// The brightness "on" should go back to: the one showing now if the panel
/// is lit, else the last one that was.
#[must_use]
pub fn lit(previous: Option<u8>, snap: &Snapshot) -> Option<u8> {
    match snap.brightness {
        Some(b) if b > 0 => Some(b),
        _ => previous,
    }
}

/// What the JSON light's command topic carries. `transition` and `flash` are
/// not offered, and ignored if they come anyway.
#[derive(Debug, Deserialize)]
struct LightCommand {
    state: String,
    #[serde(default)]
    brightness: Option<f64>,
}

/// Turn one message on a command topic into a [`Command`], checked against
/// what the entity declared: brightness `0..=255`, a scene in the list,
/// `ON`/`OFF`, `PRESS`.
///
/// `last_lit` is what a bare "on" restores.
///
/// # Errors
///
/// A sentence for the log: which topic, and what was wrong with the payload.
pub fn parse_command(topics: &Topics, topic: &str, payload: &[u8], snap: &Snapshot, last_lit: Option<u8>) -> Result<Command, String> {
    if payload.len() > MAX_COMMAND_BYTES {
        return Err(format!("{topic}: {} bytes is not a command", payload.len()));
    }
    let text = std::str::from_utf8(payload).map_err(|_| format!("{topic}: not UTF-8"))?.trim();
    if topic == topics.brightness.set {
        let cmd: LightCommand = serde_json::from_str(text).map_err(|e| format!("{topic}: `{text}`: {e}"))?;
        return match cmd.state.as_str() {
            "OFF" => Ok(Command::SetBrightness(0)),
            "ON" => match cmd.brightness {
                Some(b) if b.is_finite() && (0.0..=255.0).contains(&b) => {
                    // In range, so the cast is exact after rounding.
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    Ok(Command::SetBrightness(b.round() as u8))
                }
                Some(b) => Err(format!("{topic}: brightness {b} is outside 0..=255")),
                None => Ok(Command::SetBrightness(last_lit.unwrap_or(DEFAULT_LIT))),
            },
            other => Err(format!("{topic}: state `{other}` is neither ON nor OFF")),
        };
    }
    if topic == topics.scene.set {
        return match snap.scenes.iter().find(|s| s.as_str() == text) {
            Some(s) => Ok(Command::ApplyScene(s.clone())),
            None => Err(format!("{topic}: there is no scene called `{text}`")),
        };
    }
    if topic == topics.schedule.set {
        return match text {
            "ON" => Ok(Command::SetSchedule(true)),
            "OFF" => Ok(Command::SetSchedule(false)),
            other => Err(format!("{topic}: `{other}` is neither ON nor OFF")),
        };
    }
    if topic == topics.resume.set {
        return match text {
            "PRESS" => Ok(Command::ResumeSchedule),
            other => Err(format!("{topic}: `{other}` is not PRESS")),
        };
    }
    Err(format!("{topic}: not a command topic"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ha::{MqttConfig, PatchState, ScheduleState};
    use std::path::PathBuf;

    pub(crate) fn config() -> MqttConfig {
        MqttConfig {
            host: "broker.example".into(),
            port: 1883,
            username: None,
            password: None,
            discovery_prefix: "homeassistant".into(),
            instance: "studio".into(),
            name: "Screeny".into(),
        }
    }

    /// Compare `text` with `src/ha/snapshots/<name>`. `SCREENY_BLESS=1`
    /// writes it instead, for a change that is meant.
    pub(crate) fn snapshot_json(name: &str, text: &str) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ha/snapshots").join(name);
        let text = format!("{text}\n");
        if std::env::var("SCREENY_BLESS").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &text).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (run with SCREENY_BLESS=1 to write it)", path.display()));
        assert_eq!(text, want, "{name} changed; if that is meant, run with SCREENY_BLESS=1");
    }

    fn snap() -> Snapshot {
        Snapshot {
            patch: PatchState { id: "overland".into(), name: "Overland".into(), setting: "Dusk".into(), modified: false },
            brightness: Some(96),
            scenes: vec!["Day".into(), "Night".into()],
            scene: Some("Day".into()),
            schedule: ScheduleState {
                enabled: true,
                due: Some("Day".into()),
                until: Some("22:00".into()),
                overridden: false,
                note: None,
            },
            panel_connected: true,
        }
    }

    fn render(messages: &[Message]) -> String {
        messages.iter().map(|m| format!("{} {}", m.topic, m.payload)).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn state_when_lit_and_scheduled() {
        let topics = Topics::new(&config());
        snapshot_json("state-lit.txt", &render(&state_messages(&topics, &snap())));
    }

    #[test]
    fn state_when_dark_unscheduled_and_nothing_matches() {
        let topics = Topics::new(&config());
        let s = Snapshot {
            brightness: Some(0),
            scene: None,
            schedule: ScheduleState::default(),
            panel_connected: false,
            ..snap()
        };
        snapshot_json("state-dark.txt", &render(&state_messages(&topics, &s)));
    }

    #[test]
    fn state_when_the_brightness_is_unknown() {
        let topics = Topics::new(&config());
        let s = Snapshot { brightness: None, ..snap() };
        let light = state_messages(&topics, &s).into_iter().find(|m| m.topic == topics.brightness.state).unwrap();
        assert_eq!(light.payload, r#"{"state":null}"#);
    }

    fn parse(topic: &str, payload: &str) -> Result<Command, String> {
        let topics = Topics::new(&config());
        let topic = match topic {
            "light" => topics.brightness.set.clone(),
            "scene" => topics.scene.set.clone(),
            "schedule" => topics.schedule.set.clone(),
            "resume" => topics.resume.set.clone(),
            other => other.to_string(),
        };
        parse_command(&topics, &topic, payload.as_bytes(), &snap(), Some(40))
    }

    #[test]
    fn light_commands() {
        assert_eq!(parse("light", r#"{"state":"ON","brightness":200}"#), Ok(Command::SetBrightness(200)));
        assert_eq!(parse("light", r#"{"state":"ON","brightness":12.6}"#), Ok(Command::SetBrightness(13)));
        assert_eq!(parse("light", r#"{"state":"OFF"}"#), Ok(Command::SetBrightness(0)));
        assert_eq!(parse("light", r#"{"state":"ON"}"#), Ok(Command::SetBrightness(40)), "a bare on restores the last lit level");
        assert_eq!(parse("light", r#"{"state":"ON","brightness":255,"transition":2}"#), Ok(Command::SetBrightness(255)));
        assert!(parse("light", r#"{"state":"ON","brightness":256}"#).is_err());
        assert!(parse("light", r#"{"state":"ON","brightness":-1}"#).is_err());
        assert!(parse("light", r#"{"state":"DIM"}"#).is_err());
        assert!(parse("light", "ON").is_err(), "the JSON schema, not the default one");
    }

    #[test]
    fn a_bare_on_with_nothing_remembered_is_the_middle() {
        let topics = Topics::new(&config());
        let got = parse_command(&topics, &topics.brightness.set, br#"{"state":"ON"}"#, &snap(), None);
        assert_eq!(got, Ok(Command::SetBrightness(DEFAULT_LIT)));
    }

    #[test]
    fn scene_commands() {
        assert_eq!(parse("scene", "Night"), Ok(Command::ApplyScene("Night".into())));
        assert_eq!(parse("scene", " Night\n"), Ok(Command::ApplyScene("Night".into())), "whitespace is not part of a name");
        assert!(parse("scene", "night").is_err(), "HA sends the option exactly as declared");
        assert!(parse("scene", "Dusk").is_err());
        assert!(parse("scene", "None").is_err());
    }

    #[test]
    fn schedule_commands() {
        assert_eq!(parse("schedule", "ON"), Ok(Command::SetSchedule(true)));
        assert_eq!(parse("schedule", "OFF"), Ok(Command::SetSchedule(false)));
        assert!(parse("schedule", "on").is_err());
        assert_eq!(parse("resume", "PRESS"), Ok(Command::ResumeSchedule));
        assert!(parse("resume", "").is_err());
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(parse("screeny/studio/patch/set", "x").is_err(), "the patch is read-only");
        assert!(parse("scene", &"x".repeat(MAX_COMMAND_BYTES + 1)).is_err());
        let topics = Topics::new(&config());
        assert!(parse_command(&topics, &topics.scene.set, &[0xff, 0xfe], &snap(), None).is_err());
    }

    #[test]
    fn lit_remembers_the_last_nonzero() {
        let s = |b| Snapshot { brightness: b, ..Snapshot::default() };
        assert_eq!(lit(None, &s(Some(60))), Some(60));
        assert_eq!(lit(Some(60), &s(Some(0))), Some(60));
        assert_eq!(lit(Some(60), &s(None)), Some(60));
        assert_eq!(lit(Some(60), &s(Some(20))), Some(20));
    }
}
