//! What goes on the state topics, and what comes off the command topics.
//!
//! Pure: a [`Snapshot`] in, messages out; a topic and bytes in, a validated
//! [`Command`] or a sentence saying why not.

use super::topics::{Role, Topics};
use super::{Command, Snapshot};
use serde::{Deserialize, Serialize};

/// A command payload longer than this is not one of ours.
pub const MAX_COMMAND_BYTES: usize = 1024;
/// What "on" means when HA says only "on" and the panel has never been lit
/// in this process: the middle of the scale, never full.
pub const DEFAULT_LIT: u8 = 128;
/// What the `update` entity sends on its command topic for Install.
pub const INSTALL: &str = "INSTALL";
/// A select's state for "none of the options" - HA shows it as unknown.
pub const NO_OPTION: &str = "None";
/// The brightness slider's step: one output-enable slot of the panel's 25
/// (card 187), which is exactly 4 % of full light.
pub const PERCENT_STEP: u8 = 4;

/// The share of full light a brightness level gives, in percent: always a
/// multiple of [`PERCENT_STEP`], because the panel dims in whole slots.
#[must_use]
pub fn percent_of(level: u8) -> u8 {
    let slots = screeny_panel::model::oe_slots(level);
    // At most 25 slots, so at most 100.
    u8::try_from(slots * u32::from(PERCENT_STEP)).unwrap_or(100)
}

/// The dimmest level that gives `percent` of full light, rounded to the
/// nearest whole slot. `0` is dark; anything above it is at least one slot.
#[must_use]
pub fn level_for(percent: u8) -> u8 {
    let slots = (u32::from(percent.min(100)) + u32::from(PERCENT_STEP) / 2) / u32::from(PERCENT_STEP);
    let slots = if percent > 0 { slots.max(1) } else { 0 };
    (0..=u8::MAX).find(|l| screeny_panel::model::oe_slots(*l) >= slots).unwrap_or(u8::MAX)
}

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

/// What HA's `update` entity reads off its state topic: the keys of
/// <https://www.home-assistant.io/integrations/update.mqtt/>. Every key is
/// always there, so a retained message never leaves HA holding a value from
/// an earlier one - **except a version nobody knows**.
///
/// **Card 368: HA validates this against a schema (`MQTT_JSON_UPDATE_SCHEMA`),
/// and a `null` in a string key fails the whole message** ("Schema
/// violation ... ignored"), so the entity stays *Unknown* for ever. A version
/// HA has not been told is therefore left out, and `release_summary` is the
/// empty string rather than `null`. Only `update_percentage` may be `null`.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct UpdateState<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_version: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_version: Option<&'a str>,
    title: &'static str,
    release_summary: String,
    in_progress: bool,
    update_percentage: Option<u8>,
}

/// HA truncates a release summary at 255 characters; so does this, on a
/// character boundary.
const SUMMARY_MAX: usize = 255;

impl<'a> UpdateState<'a> {
    fn of(f: &'a super::FirmwareState) -> Self {
        UpdateState {
            installed_version: f.installed.as_deref(),
            latest_version: f.latest.as_deref(),
            title: "Screeny firmware",
            release_summary: f.failed.as_ref().map_or_else(String::new, |why| format!("The last update failed: {why}").chars().take(SUMMARY_MAX).collect()),
            in_progress: f.in_progress,
            update_percentage: f.percent,
        }
    }
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
    let patch = Message { topic: topics.patch.state.clone(), payload: json(&snap.patch) };
    let picture = Message { topic: topics.picture.state.clone(), payload: snap.picture.clone().unwrap_or_else(|| NO_OPTION.to_string()) };
    let brightness = Message { topic: topics.brightness.state.clone(), payload: json(&light) };
    let level = Message { topic: topics.level.state.clone(), payload: snap.brightness.map_or_else(|| NO_OPTION.to_string(), |b| percent_of(b).to_string()) };
    let panel = Message { topic: topics.panel.state.clone(), payload: on_off(snap.panel_connected) };
    // The channel this panel is on, by the label its select shows.
    let on = snap.channel.and_then(|id| snap.channels.iter().find(|c| c.id == id)).map_or_else(|| NO_OPTION.to_string(), |c| c.label.clone());
    let channel = Message { topic: topics.channel.state.clone(), payload: on };
    let firmware = Message { topic: topics.firmware.state.clone(), payload: json(&UpdateState::of(&snap.firmware)) };
    match topics.role {
        // The five it always had, in their order, and the channel select after,
        // and card 364's firmware last.
        Role::First => vec![patch, brightness, level, picture, panel, channel, firmware],
        Role::Panel => vec![brightness, level, panel, channel, firmware],
        Role::Channel => vec![patch, picture],
    }
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
/// what the entity declared: brightness `0..=255` or `0..=100` %, a picture
/// in the list.
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
    if topic == topics.level.set {
        let p: f64 = text.parse().map_err(|_| format!("{topic}: `{text}` is not a number"))?;
        if !p.is_finite() || !(0.0..=100.0).contains(&p) {
            return Err(format!("{topic}: {text} is outside 0..=100"));
        }
        // In range, so the cast is exact after rounding.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        return Ok(Command::SetBrightness(level_for(p.round() as u8)));
    }
    if topic == topics.firmware.set {
        // HA's default `payload_install`; anything else is not Install.
        return if text == INSTALL { Ok(Command::InstallFirmware) } else { Err(format!("{topic}: `{text}` is not {INSTALL}")) };
    }
    if topic == topics.channel.set {
        return match snap.channels.iter().find(|c| c.label == text) {
            Some(c) => Ok(Command::MoveToChannel(c.id)),
            None => Err(format!("{topic}: there is no channel called `{text}`")),
        };
    }
    if topic == topics.picture.set {
        return match snap.pictures.iter().find(|p| p.label == text) {
            Some(p) => Ok(Command::ShowPicture { patch: p.patch.clone(), setting: p.setting.clone() }),
            None => Err(format!("{topic}: there is no picture called `{text}`")),
        };
    }
    Err(format!("{topic}: not a command topic"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ha::{MqttConfig, PatchState, Picture};
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

    pub(crate) fn pictures() -> Vec<Picture> {
        let p = |name: &str, id: &str, setting: &str| Picture { label: Picture::label(name, setting), patch: id.into(), setting: setting.into() };
        vec![
            p("Overland", "overland", "Default"),
            p("Overland", "overland", "Dusk"),
            p("Vesta", "vesta", "Default"),
            p("Vesta", "vesta", "Wall Clock"),
        ]
    }

    fn snap() -> Snapshot {
        Snapshot {
            patch: PatchState { id: "overland".into(), name: "Overland".into(), setting: "Dusk".into(), modified: false },
            brightness: Some(96),
            pictures: pictures(),
            picture: Some("Overland · Dusk".into()),
            panel_connected: true,
            channels: crate::ha::ChannelOption::all(&[(1, "Channel 1".into()), (2, "Drawing room".into())]),
            channel: Some(2),
            firmware: crate::ha::FirmwareState { installed: Some("0.10.0".into()), latest: Some("0.11.0".into()), ..Default::default() },
        }
    }

    fn render(messages: &[Message]) -> String {
        messages.iter().map(|m| format!("{} {}", m.topic, m.payload)).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn state_when_lit_and_scheduled() {
        let topics = Topics::new(&config());
        let all = state_messages(&topics, &snap());
        // Card 355 added the channel select's state after them, and card 364
        // the firmware's last. The five before are card 308-311's, byte for
        // byte, and pinned as they were.
        let (firmware, rest) = all.split_last().unwrap();
        let (channel, old) = rest.split_last().unwrap();
        assert_eq!((channel.topic.as_str(), channel.payload.as_str()), ("screeny/studio/channel/state", "Drawing room"));
        snapshot_json("state-lit.txt", &render(old));
        assert_eq!(firmware.topic, "screeny/studio/firmware/state");
        snapshot_json("firmware-state.txt", &render(std::slice::from_ref(firmware)));
    }

    /// Card 364: what HA's `update` entity is told in each situation. Every key
    /// is always there, so a retained message never leaves HA holding a value
    /// from an earlier one.
    #[test]
    fn the_firmware_state_in_each_situation() {
        use crate::ha::FirmwareState;
        let topics = Topics::new(&config());
        let state = |f: FirmwareState| {
            let all = state_messages(&topics, &Snapshot { firmware: f, ..snap() });
            let m = all.into_iter().find(|m| m.topic == topics.firmware.state).unwrap();
            serde_json::from_str::<serde_json::Value>(&m.payload).unwrap()
        };
        let available = FirmwareState { installed: Some("0.10.0".into()), latest: Some("0.11.0".into()), ..FirmwareState::default() };
        let v = state(available.clone());
        assert_eq!((v["installed_version"].as_str(), v["latest_version"].as_str()), (Some("0.10.0"), Some("0.11.0")));
        assert_eq!(v["in_progress"], false);
        let going = state(FirmwareState { in_progress: true, percent: Some(42), ..available.clone() });
        assert_eq!((going["in_progress"].clone(), going["update_percentage"].clone()), (true.into(), 42.into()));
        let waiting = state(FirmwareState { in_progress: true, ..available.clone() });
        assert!(waiting["in_progress"] == true && waiting["update_percentage"].is_null(), "restarting has no percentage: {waiting}");
        let failed = state(FirmwareState { failed: Some("the panel refused the image".into()), ..available });
        assert_eq!(failed["release_summary"], "The last update failed: the panel refused the image");
        let up_to_date = state(FirmwareState { installed: Some("0.11.0".into()), latest: Some("0.11.0".into()), ..FirmwareState::default() });
        assert_eq!(up_to_date["installed_version"], up_to_date["latest_version"], "HA shows it up to date");
        let unknown = state(FirmwareState::default());
        assert!(unknown.get("installed_version").is_none() && unknown.get("latest_version").is_none(), "left out, never null: {unknown}");
        // HA's MQTT_JSON_UPDATE_SCHEMA: these keys are `cv.string`, which
        // refuses null, and one bad key drops the whole message (card 368).
        for f in [up_to_date_state(), FirmwareState::default()] {
            let v = state(f);
            for key in ["installed_version", "latest_version", "title", "release_summary"] {
                assert!(v.get(key).is_none_or(serde_json::Value::is_string), "{key} in {v}");
            }
            assert_eq!(v["release_summary"], "");
            assert!(v["in_progress"].is_boolean());
        }
        let long = state(FirmwareState { failed: Some("x".repeat(1000)), ..FirmwareState::default() });
        assert_eq!(long["release_summary"].as_str().unwrap().chars().count(), SUMMARY_MAX);
    }

    fn up_to_date_state() -> crate::ha::FirmwareState {
        crate::ha::FirmwareState { installed: Some("0.11.0".into()), latest: Some("0.11.0".into()), ..Default::default() }
    }

    #[test]
    fn install_is_the_only_firmware_command() {
        let topics = Topics::new(&config());
        let parse = |p: &str| parse_command(&topics, &topics.firmware.set, p.as_bytes(), &snap(), None);
        assert_eq!(parse("INSTALL"), Ok(Command::InstallFirmware));
        assert_eq!(parse(" INSTALL\n"), Ok(Command::InstallFirmware));
        assert!(parse("install").is_err() && parse("").is_err() && parse("ON").is_err());
        let channel = Topics::for_channel(&config(), 2);
        assert!(!channel.command_topics().contains(&topics.firmware.set.as_str()), "a channel has no firmware");
    }

    #[test]
    fn state_when_dark_unscheduled_and_nothing_matches() {
        let topics = Topics::new(&config());
        let s = Snapshot {
            brightness: Some(0),
            picture: None,
            panel_connected: false,
            ..snap()
        };
        let all = state_messages(&topics, &s);
        // Without the channel select's state and the firmware's, which are later additions.
        snapshot_json("state-dark.txt", &render(&all[..all.len() - 2]));
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
            "picture" => topics.picture.set.clone(),
            "level" => topics.level.set.clone(),
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
    fn picture_commands() {
        let show = |patch: &str, setting: &str| Command::ShowPicture { patch: patch.into(), setting: setting.into() };
        assert_eq!(parse("picture", "Vesta"), Ok(show("vesta", "Default")), "a bare patch name is its Default");
        assert_eq!(parse("picture", "Vesta · Wall Clock"), Ok(show("vesta", "Wall Clock")));
        assert_eq!(parse("picture", " Overland · Dusk\n"), Ok(show("overland", "Dusk")), "whitespace is not part of a label");
        assert!(parse("picture", "vesta").is_err(), "HA sends the option exactly as declared");
        assert!(parse("picture", "Vesta · Default").is_err(), "Default is the bare name");
        assert!(parse("picture", "Flock").is_err(), "not in the list");
        assert!(parse("picture", "None").is_err());
        assert!(parse("screeny/studio/scene/set", "Day").is_err(), "scenes are retired");
    }

    #[test]
    fn channel_commands() {
        assert_eq!(parse("screeny/studio/channel/set", "Drawing room"), Ok(Command::MoveToChannel(2)));
        assert_eq!(parse("screeny/studio/channel/set", " Channel 1\n"), Ok(Command::MoveToChannel(1)));
        assert!(parse("screeny/studio/channel/set", "Channel 3").is_err(), "not in the list");
        assert!(parse("screeny/studio/channel/set", "None").is_err());
        // Two channels with one name are told apart by their ids.
        let dup = crate::ha::ChannelOption::all(&[(1, "Hall".into()), (2, "Hall".into()), (3, "Den".into())]);
        let labels: Vec<&str> = dup.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Hall (1)", "Hall (2)", "Den"]);
    }

    #[test]
    fn a_channels_device_says_its_patch_and_picture_and_a_later_panels_no_picture() {
        let cfg = config();
        let ch = state_messages(&Topics::for_channel(&cfg, 2), &snap());
        let topics: Vec<&str> = ch.iter().map(|m| m.topic.as_str()).collect();
        assert_eq!(topics, ["screeny/studio/ch2/patch/state", "screeny/studio/ch2/picture/state"]);
        let panel = state_messages(&Topics::for_panel(&cfg, Some("b")), &snap());
        let topics: Vec<&str> = panel.iter().map(|m| m.topic.as_str()).collect();
        assert_eq!(topics, ["screeny/studio/b/brightness/state", "screeny/studio/b/level/state", "screeny/studio/b/panel/state", "screeny/studio/b/channel/state", "screeny/studio/b/firmware/state"]);
        assert_eq!(panel[3].payload, "Drawing room");
        let none = state_messages(&Topics::for_panel(&cfg, Some("b")), &Snapshot { channel: None, ..snap() });
        assert_eq!(none[3].payload, NO_OPTION);
    }

    #[test]
    fn labels() {
        assert_eq!(Picture::label("Vesta", "Default"), "Vesta");
        assert_eq!(Picture::label("Vesta", "default"), "Vesta");
        assert_eq!(Picture::label("Metaballs", "Lava"), "Metaballs · Lava");
    }

    #[test]
    fn level_commands() {
        assert_eq!(parse("level", "0"), Ok(Command::SetBrightness(0)));
        assert_eq!(parse("level", "100"), Ok(Command::SetBrightness(level_for(100))));
        assert_eq!(parse("level", "4"), Ok(Command::SetBrightness(level_for(4))));
        assert_eq!(parse("level", "40.0"), Ok(Command::SetBrightness(level_for(40))));
        assert!(parse("level", "101").is_err());
        assert!(parse("level", "-4").is_err());
        assert!(parse("level", "dim").is_err());
        assert!(parse("screeny/studio/schedule/set", "ON").is_err(), "the timetable is Home Assistant's now");
    }

    /// Card 310: the slider's percent is the panel's own resolution - one
    /// output-enable slot in 25 - so every step is a real change and a level
    /// read back is the percent that was set.
    #[test]
    fn percent_and_level_are_the_panels_own_steps() {
        let stops = crate::page::brightness_stops();
        assert_eq!(stops.len(), 26, "dark and 25 slots");
        for (slots, stop) in stops.iter().enumerate() {
            let percent = u8::try_from(slots).unwrap() * PERCENT_STEP;
            assert_eq!(percent_of(*stop), percent);
            assert_eq!(level_for(percent), *stop, "{percent} %");
        }
        assert_eq!(level_for(1), stops[1], "above zero is never dark");
        assert_eq!(level_for(5), stops[1], "rounds to the nearest step");
        assert_eq!(level_for(7), stops[2]);
        assert_eq!(percent_of(255), 100);
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(parse("screeny/studio/patch/set", "x").is_err(), "the patch is read-only");
        assert!(parse("picture", &"x".repeat(MAX_COMMAND_BYTES + 1)).is_err());
        let topics = Topics::new(&config());
        assert!(parse_command(&topics, &topics.picture.set, &[0xff, 0xfe], &snap(), None).is_err());
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
