//! Every panel and every channel, and what ties them (cards 350, 353).
//!
//! **Channels own the picture** (`docs/design/studio-vision.md`, "Several
//! panels: channels own the picture", normative). A [`Channel`] is explicit:
//! an id and a name, a picture, its output settings, and one render, one
//! pipeline and one encode per tick. A [`Panel`] is a member of exactly one
//! channel and is sent that channel's encoded frames byte for byte, so panels
//! on one channel are frame-for-frame identical and none of them is the
//! master. This type owns both collections and is the only thing that changes
//! which panel is on which channel, so the topology has one lock:
//!
//! - there is always **Channel 1** ([`HOME_CHANNEL`]); it cannot be deleted,
//!   it is what a route without `channel` means, and **a new panel joins it**
//!   and lights up straight away, mirroring it;
//! - **"New channel"** ([`Panels::new_channel`]) makes another, a copy of an
//!   existing one by default so moving a panel onto it is seamless; a channel
//!   can be renamed and deleted, and deleting one moves its panels to
//!   Channel 1. A channel with no panels is kept, and renders only while
//!   somebody watches it;
//! - **moving a panel** ([`Panels::move_panel`]) fades that panel alone from
//!   the old channel's picture to the new one's over [`FADE_MANUAL`];
//! - **picking a picture changes the channel** ([`Panels::pick`]) - every
//!   panel on it, which is the point. There are no implicit rules any more:
//!   card 350's join / change / split, *Same as* and *Detach* are retired.
//!
//! Lock order, everywhere: this type's lock, then a panel's channel slot, then
//! a channel's member list, then a panel's fade, then a channel's stage. A
//! render thread only ever takes the last three, never while holding a
//! member list.

use screeny_art::output::PanelStatus;
use screeny_art::patch::{Params, PatchDef, Playing};
use screeny_art::Output;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::channel::{fade_len, find_patch, shared_output, Channel, ChannelId, SharedOutput, FADE_MANUAL};
use crate::devices::Registry;
use crate::page::{SocketMeter, StudioState};
use crate::panel::{Panel, SendCounts};
use crate::state::{self, SharedMemory, StoredChannel, StoredPanel, DEFAULT_SETTING, HOME_CHANNEL};

/// What `/api/v1/status` says about one panel and the picture it shows.
///
/// Card 106's per-device `player`, kept in shape so that scripts and tests
/// written against it keep working: the picture's half is the channel's, the
/// device's half the panel's. Panels on one channel report the same picture,
/// the same `ticks`, because it is the same render.
#[derive(Clone, Debug, Serialize)]
pub struct PlayerStatus {
    pub device: String,
    /// Panel output: false means the link is released and the panel is on its
    /// own idle screen.
    pub on: bool,
    pub patch: String,
    pub patch_name: String,
    pub seed: u32,
    /// Only what has been set away from the patch's defaults.
    pub params: BTreeMap<String, f32>,
    /// The one rate, `screeny_art::FPS` (card 161). Reported, never set.
    pub fps: f64,
    /// Retired (card 302); always false.
    pub paused: bool,
    /// Retired (card 302); always 1.0.
    pub speed: f64,
    pub brightness: Option<u8>,
    /// Its channel's output settings (card 353: the channel's, not the panel's).
    pub output: Output,
    /// True when its channel's render thread is alive and ticking.
    pub running: bool,
    /// True when this is the first panel (the first adopted).
    pub focused: bool,
    /// The rate its channel's render loop is actually achieving.
    pub fps_measured: f32,
    /// What a composing patch says it is performing.
    pub playing: Option<Playing>,
    pub health: PlayerHealth,
    /// The link, or `None` when there is none (output off).
    pub panel: Option<PanelStatus>,
    /// The channel it is on.
    pub channel: ChannelId,
    /// That channel's name.
    pub channel_name: String,
    /// The named setting the picture came from.
    pub setting: String,
    /// Whether the picture has moved away from that setting.
    pub modified: bool,
    /// The other panels on the same channel.
    pub shared_with: Vec<String>,
    /// Card 353: how its frames were made - the channel's shared bytes, or
    /// its own (its fade; a frame its session could not take as encoded).
    pub sends: SendCounts,
}

/// Everything a panel and its picture have been through, as one block.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PlayerHealth {
    /// Frames its channel has rendered, across cores.
    pub ticks: u64,
    pub panics: u64,
    pub stalls: u64,
    pub restarts: u64,
    pub abandoned: u64,
    pub fell_back_from: Option<String>,
    pub refused: Vec<String>,
    pub gave_up: Option<String>,
    /// Seconds since its channel last completed a frame.
    pub last_tick_ago: Option<f64>,
    /// Seconds since a frame last reached the wire.
    pub last_frame_ago: Option<f64>,
    pub sessions: u64,
    pub link_ups: u64,
    pub reconnects: u64,
    pub brightness_applied: Option<u8>,
    pub brightness_cap: Option<u8>,
    /// The last thing that went wrong, with the link or the picture.
    pub last_error: Option<String>,
}

/// One panel, as the Panel screen and the overview draw it (cards 350, 353):
/// `GET /api/v1/panels` and the socket's `panels` message.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PanelSummary {
    /// The device id: what every `panel` parameter takes.
    pub device: String,
    /// What to call it: the name set here, else the device's own, else its
    /// instance name, else the id.
    pub name: String,
    /// True for the first panel adopted.
    pub first: bool,
    /// Panel output.
    pub on: bool,
    /// The brightness policy, 0-255, or `null` for "whatever the device has".
    pub brightness: Option<u8>,
    /// `off` (output switched off), or the link's own state: `up`,
    /// `connecting`, `waiting`, `closed`.
    pub link: String,
    /// True only while the link is `up`.
    pub connected: bool,
    /// The channel it is on - always one (card 353).
    pub channel: ChannelId,
    /// That channel's name.
    pub channel_name: String,
    /// What its channel shows.
    pub picture: PictureSummary,
    /// The other panels on the same channel, by device id.
    pub shared_with: Vec<String>,
    /// True for the two seconds it fades onto its channel, when its frames
    /// are its own.
    pub fading: bool,
}

/// One channel, as the Picture screen's row draws it (card 353):
/// `GET /api/v1/channels` and the socket's `channels` message.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChannelSummary {
    pub id: ChannelId,
    pub name: String,
    /// True for Channel 1: the one that cannot be deleted, and the one a new
    /// panel joins.
    pub home: bool,
    pub picture: PictureSummary,
    /// How its frames are finished - every member's.
    pub output: Output,
    /// The panels on it, by device id, in the order they joined.
    pub panels: Vec<String>,
}

/// A picture, in the overview's words.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PictureSummary {
    pub patch: String,
    pub patch_name: String,
    /// Always a name: `Default` when it is on the patch's own.
    pub setting: String,
    pub modified: bool,
}

struct Inner {
    /// In the order they were adopted.
    panels: Vec<Arc<Panel>>,
    channels: BTreeMap<ChannelId, Arc<Channel>>,
    next: ChannelId,
    /// Deleted channels that a panel is still fading away from: shut down
    /// once nothing needs them ([`Panels::sweep`]).
    retired: Vec<Arc<Channel>>,
}

/// Every panel and every channel.
pub struct Panels {
    inner: Mutex<Inner>,
    memory: SharedMemory,
    meter: Arc<SocketMeter>,
    /// Card 356: the studio's one output stage, shared with every channel.
    output: SharedOutput,
    faults: bool,
    /// Start a channel's render thread as soon as it is made. Off only in the
    /// unit tests that look at the topology and nothing else.
    autostart: bool,
}

impl Panels {
    #[must_use]
    pub fn new(memory: SharedMemory, faults: bool) -> Self {
        let panels = Panels {
            inner: Mutex::new(Inner { panels: Vec::new(), channels: BTreeMap::new(), next: HOME_CHANNEL + 1, retired: Vec::new() }),
            memory,
            meter: Arc::new(SocketMeter::default()),
            output: shared_output(state::default_output()),
            faults,
            autostart: true,
        };
        panels.ensure_home();
        panels
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The studio's one library of named settings, shared by every channel.
    #[must_use]
    pub fn memory(&self) -> SharedMemory {
        self.memory.clone()
    }

    /// **The studio's output settings** (card 356): how every channel's
    /// frames are finished.
    #[must_use]
    pub fn output(&self) -> Output {
        *self.output.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Change it for every channel; picked up on the next frame.
    pub fn set_output(&self, output: Output) {
        *self.output.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = output;
    }

    /// What every preview socket has cost.
    #[must_use]
    pub fn meter(&self) -> Arc<SocketMeter> {
        Arc::clone(&self.meter)
    }

    /// Whether the fault patches are offered.
    #[must_use]
    pub fn faults(&self) -> bool {
        self.faults
    }

    // ---------------------------------------------------------- reading ---

    /// The first panel adopted, if there is any.
    #[must_use]
    pub fn first(&self) -> Option<Arc<Panel>> {
        self.lock().panels.first().cloned()
    }

    #[must_use]
    pub fn get(&self, device: &str) -> Option<Arc<Panel>> {
        self.lock().panels.iter().find(|p| p.device() == device).cloned()
    }

    /// Every panel, in the order they were adopted.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<Panel>> {
        self.lock().panels.clone()
    }

    /// Their device ids, in the same order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.lock().panels.iter().map(|p| p.device()).collect()
    }

    /// Every channel, by id - Channel 1 first.
    #[must_use]
    pub fn channels(&self) -> Vec<Arc<Channel>> {
        self.lock().channels.values().cloned().collect()
    }

    /// A channel by id.
    #[must_use]
    pub fn channel(&self, id: ChannelId) -> Option<Arc<Channel>> {
        self.lock().channels.get(&id).cloned()
    }

    /// **Channel 1**, which always exists.
    #[must_use]
    pub fn home(&self) -> Arc<Channel> {
        self.ensure_home()
    }

    /// Is this the first panel?
    #[must_use]
    pub fn is_first(&self, panel: &Arc<Panel>) -> bool {
        self.lock().panels.first().is_some_and(|p| Arc::ptr_eq(p, panel))
    }

    /// What to write to the state file.
    #[must_use]
    pub fn stored(&self) -> (Vec<StoredPanel>, Vec<StoredChannel>) {
        let inner = self.lock();
        let panels = inner.panels.iter().map(|p| p.stored()).collect();
        let channels = inner.channels.values().map(|c| c.stored()).collect();
        (panels, channels)
    }

    // ---------------------------------------------------- the collection ---

    fn ensure_home(&self) -> Arc<Channel> {
        let mut inner = self.lock();
        if let Some(c) = inner.channels.get(&HOME_CHANNEL) {
            return Arc::clone(c);
        }
        let stored = StoredChannel { id: HOME_CHANNEL, name: state::default_channel_name(HOME_CHANNEL), ..StoredChannel::default() };
        self.insert_locked(&mut inner, stored)
    }

    fn insert_locked(&self, inner: &mut Inner, stored: StoredChannel) -> Arc<Channel> {
        inner.next = inner.next.max(stored.id.saturating_add(1));
        let c = Channel::new(stored, self.faults, &self.meter, &self.output);
        inner.channels.insert(c.id(), Arc::clone(&c));
        if self.autostart {
            c.ensure_running();
        }
        c
    }

    /// Adopt what the state file had. Render threads start with
    /// [`Panels::start`].
    pub fn load(&self, panels: Vec<StoredPanel>, channels: Vec<StoredChannel>) {
        {
            let mut inner = self.lock();
            for c in channels {
                if c.id == 0 {
                    continue;
                }
                inner.next = inner.next.max(c.id.saturating_add(1));
                if let Some(old) = inner.channels.insert(c.id, Channel::new(c, self.faults, &self.meter, &self.output)) {
                    old.shutdown();
                }
            }
        }
        let home = self.ensure_home();
        let mut inner = self.lock();
        for sp in panels {
            if sp.device.is_empty() || inner.panels.iter().any(|p| p.device() == sp.device) {
                continue;
            }
            let panel = Panel::new(&sp);
            let channel = inner.channels.get(&sp.channel).cloned().unwrap_or_else(|| Arc::clone(&home));
            // Every panel comes up fading in from black, as a studio's first
            // picture always has.
            panel.switch_channel(&channel, f64::from(FADE_MANUAL));
            inner.panels.push(panel);
        }
    }

    /// Start every channel's render thread that is not running.
    pub fn start(&self) {
        for c in self.channels() {
            c.ensure_running();
        }
    }

    /// **Adopt a device as a panel** (card 353: on Channel 1, lit, fading in
    /// from black and then mirroring it). True when the panel is new.
    pub fn adopt(&self, device: &str) -> bool {
        if device.is_empty() {
            return false;
        }
        let home = self.ensure_home();
        let mut inner = self.lock();
        if inner.panels.iter().any(|p| p.device() == device) {
            return false;
        }
        let panel = Panel::new(&StoredPanel { device: device.to_string(), ..StoredPanel::default() });
        panel.switch_channel(&home, f64::from(FADE_MANUAL));
        inner.panels.push(panel);
        true
    }

    /// **The panel for a device somebody has named**: the one there is, or a
    /// new one on Channel 1.
    pub fn attach(&self, device: &str) -> Arc<Panel> {
        self.adopt(device);
        self.get(device).unwrap_or_else(|| {
            // Unreachable: `adopt` just made it, and only an empty id is refused
            // - which no caller names. A panel on Channel 1 all the same.
            let p = Panel::new(&StoredPanel { device: device.to_string(), ..StoredPanel::default() });
            p.switch_channel(&self.home(), 0.0);
            p
        })
    }

    /// A device told us its real id, or a typed address resolved: the panel
    /// is **renamed in place**, keeping its channel, its link and its place
    /// in the order. If the destination already had a panel, the one that
    /// was named first wins and the other goes.
    pub fn rekey(&self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let mut inner = self.lock();
        if !inner.panels.iter().any(|p| p.device() == from) {
            return;
        }
        if let Some(i) = inner.panels.iter().position(|p| p.device() == to) {
            let old = inner.panels.remove(i);
            old.drop_channel();
            old.shutdown();
        }
        if let Some(p) = inner.panels.iter().find(|p| p.device() == from) {
            p.rename(to);
        }
    }

    /// Forget a panel: its link is released. Its channel stays.
    pub fn remove(&self, device: &str) {
        let mut inner = self.lock();
        if let Some(i) = inner.panels.iter().position(|p| p.device() == device) {
            let p = inner.panels.remove(i);
            p.drop_channel();
            p.shutdown();
        }
    }

    /// Shut down every deleted channel nothing needs any more: no panel is
    /// still fading away from it.
    pub fn sweep(&self) {
        let mut inner = self.lock();
        inner.retired.retain(|c| {
            if c.unused() {
                c.shutdown();
                false
            } else {
                true
            }
        });
    }

    // ------------------------------------------------------ the channels ---

    /// **A new channel**: a copy of `from` - its picture and its working copy,
    /// so moving a panel onto it is seamless - or of
    /// Channel 1 without one, called `name` or "Channel <id>". It has no
    /// panels until somebody moves one onto it.
    ///
    /// # Errors
    ///
    /// A name that cannot be a name.
    pub fn new_channel(&self, name: Option<&str>, from: Option<&Arc<Channel>>) -> Result<Arc<Channel>, String> {
        let name = name.map(str::trim).filter(|n| !n.is_empty()).map(state::check_channel_name).transpose()?;
        let from = from.cloned().unwrap_or_else(|| self.home());
        let mut inner = self.lock();
        let id = inner.next;
        let stored = StoredChannel { id, name: name.unwrap_or_else(|| state::default_channel_name(id)), ..from.stored() };
        Ok(self.insert_locked(&mut inner, stored))
    }

    /// Call a channel something else.
    ///
    /// # Errors
    ///
    /// A name that cannot be a name.
    pub fn rename_channel(&self, channel: &Arc<Channel>, name: &str) -> Result<(), String> {
        let name = state::check_channel_name(name)?;
        channel.set_name(&name);
        Ok(())
    }

    /// **Delete a channel**: its panels move to Channel 1 (each with its
    /// fade), and it goes. Channel 1 cannot be deleted.
    ///
    /// # Errors
    ///
    /// For Channel 1.
    pub fn delete_channel(&self, channel: &Arc<Channel>) -> Result<(), String> {
        if channel.id() == HOME_CHANNEL {
            return Err("Channel 1 is the one every new panel joins, so it cannot be deleted.".into());
        }
        let home = self.home();
        let mut inner = self.lock();
        let Some(gone) = inner.channels.remove(&channel.id()) else {
            return Ok(());
        };
        for panel in gone.followers() {
            panel.switch_channel(&home, f64::from(FADE_MANUAL));
        }
        // Kept until the panels that were on it have faded away from it.
        inner.retired.push(gone);
        Ok(())
    }

    /// **Move a panel to another channel**: it fades from the old channel's
    /// picture to the new one's over `fade` (absent: [`FADE_MANUAL`]), and is
    /// then on the new channel's shared bytes. The same channel changes
    /// nothing.
    pub fn move_panel(&self, panel: &Arc<Panel>, to: &Arc<Channel>, fade: Option<f32>) {
        let _inner = self.lock();
        if panel.channel().is_some_and(|c| Arc::ptr_eq(&c, to)) {
            return;
        }
        panel.switch_channel(to, fade_len(fade));
    }

    /// **Pick a picture for a channel**: `def` on the setting named `setting`
    /// (empty or `Default` is Default). Every panel on the channel changes,
    /// with the channel's 2 s fade - that is the point. A channel already
    /// showing exactly that, untouched, is left alone.
    ///
    /// # Errors
    ///
    /// If the patch has no setting by that name.
    pub fn pick(&self, channel: &Arc<Channel>, def: &'static PatchDef, setting: &str, fade: Option<f32>) -> Result<(), String> {
        let (work, repaired) = self.memory.usable_setting(def, setting)?;
        if !repaired.is_empty() {
            // The `repaired` voice, said once per load rather than per value:
            // a setting older than the patch is what this is for.
            eprintln!("studio: `{}` loading `{}`: {}", def.id, setting.trim(), repaired.join("; "));
        }
        if channel.shows(def.id, setting, &self.memory) {
            return Ok(());
        }
        channel.show(def, setting, work, fade);
        Ok(())
    }

    /// A setting was renamed: every channel on it follows the name.
    pub fn setting_renamed(&self, patch: &str, from: &str, to: &str) {
        for c in self.channels() {
            if c.patch() == patch && state::same_setting(&c.setting(), from) {
                c.set_setting_name(to);
            }
        }
    }

    /// A setting was deleted: every channel on it is now on Default, and says
    /// so by reading as modified. What is playing does not change.
    pub fn setting_deleted(&self, patch: &str, name: &str) {
        self.setting_renamed(patch, name, "");
    }

    /// Stop everything: every render thread ends and every panel is released
    /// with `FINAL`.
    pub fn shutdown(&self) {
        let inner = self.lock();
        for c in inner.channels.values().chain(inner.retired.iter()) {
            c.shutdown();
        }
        for p in &inner.panels {
            p.shutdown();
        }
    }

    // ------------------------------------------------------------ views ---

    /// What the page draws itself from, for one channel - seen from `from`, a
    /// panel on it, when the request named one (it decides `device` and `on`;
    /// otherwise they are the channel's first member's).
    #[must_use]
    pub fn state_of(&self, channel: &Arc<Channel>, from: Option<&Arc<Panel>>) -> StudioState {
        let stored = channel.stored();
        let members = channel.follower_ids();
        let who = from.cloned().or_else(|| channel.followers().into_iter().next());
        let (device, on) = who.map_or((String::new(), false), |p| (p.device(), p.cfg().on));
        let def = find_patch(&stored.patch, self.faults);
        // Every parameter at its effective value: the page draws one control
        // per parameter and reads its position from here.
        let params = match def {
            Some(def) => {
                let mut p = Params::defaults(def.params);
                for (id, v) in &stored.params {
                    p.set(def.params, id, *v);
                }
                p.iter().map(|(k, v)| (k.to_string(), v)).collect()
            }
            None => BTreeMap::new(),
        };
        let (settings, modified) = match def {
            Some(def) => (self.memory.setting_names(def.id), channel.modified(&self.memory)),
            None => (Vec::new(), false),
        };
        StudioState {
            patch: stored.patch,
            seed: stored.seed,
            params,
            output: self.output(),
            paused: false,
            speed: 1.0,
            fps: screeny_art::FPS,
            on,
            shared_with: members.iter().filter(|d| **d != device).cloned().collect(),
            device,
            setting: if stored.setting.is_empty() { DEFAULT_SETTING.to_string() } else { stored.setting },
            settings,
            modified,
            channel: channel.id(),
            channel_name: stored.name,
            panels: members,
        }
    }

    /// Everything `/api/v1/status` shows about one panel.
    #[must_use]
    pub fn status_of(&self, panel: &Arc<Panel>) -> PlayerStatus {
        let cfg = panel.cfg();
        let link = panel.link_health();
        let channel = panel.channel().unwrap_or_else(|| self.home());
        let s = channel.status();
        let h = s.health;
        let health = PlayerHealth {
            ticks: s.ticks,
            panics: h.panics,
            stalls: h.stalls,
            restarts: h.restarts,
            abandoned: h.abandoned,
            fell_back_from: h.fell_back_from,
            refused: h.refused,
            gave_up: h.gave_up,
            last_tick_ago: s.last_tick_ago,
            last_frame_ago: link.last_frame_ago,
            sessions: link.sessions,
            link_ups: link.link_ups,
            reconnects: link.reconnects,
            brightness_applied: link.brightness_applied,
            brightness_cap: link.brightness_cap,
            last_error: link.last_error.clone().or(h.last_error),
        };
        PlayerStatus {
            device: cfg.device.clone(),
            on: cfg.on,
            patch_name: find_patch(&s.stored.patch, self.faults).map_or("", |d| d.name).to_string(),
            patch: s.stored.patch,
            seed: s.stored.seed,
            params: s.stored.params,
            fps: screeny_art::FPS,
            paused: false,
            speed: 1.0,
            brightness: cfg.brightness,
            output: self.output(),
            running: s.running,
            focused: self.is_first(panel),
            fps_measured: s.fps_measured,
            playing: s.playing,
            health,
            panel: panel.link_status(),
            channel: s.id,
            channel_name: s.stored.name,
            setting: if s.stored.setting.is_empty() { DEFAULT_SETTING.to_string() } else { s.stored.setting },
            modified: channel.modified(&self.memory),
            shared_with: channel.follower_ids().into_iter().filter(|d| *d != cfg.device).collect(),
            sends: panel.sends(),
        }
    }

    fn picture_of(&self, channel: &Arc<Channel>) -> PictureSummary {
        let s = channel.stored();
        PictureSummary {
            patch_name: find_patch(&s.patch, self.faults).map_or("", |d| d.name).to_string(),
            patch: s.patch,
            setting: if s.setting.is_empty() { DEFAULT_SETTING.to_string() } else { s.setting },
            modified: channel.modified(&self.memory),
        }
    }

    /// Every panel, first adopted first.
    #[must_use]
    pub fn summaries(&self, devices: &Registry) -> Vec<PanelSummary> {
        let all = self.all();
        all.iter()
            .enumerate()
            .map(|(i, p)| {
                let cfg = p.cfg();
                let channel = p.channel().unwrap_or_else(|| self.home());
                let link = p.link_status();
                let link_word = match &link {
                    _ if !cfg.on => "off".to_string(),
                    Some(l) => l.state.to_string(),
                    None => "connecting".to_string(),
                };
                PanelSummary {
                    name: devices.get(&cfg.device).map_or_else(|| cfg.device.clone(), |d| d.label()),
                    first: i == 0,
                    on: cfg.on,
                    brightness: cfg.brightness,
                    connected: link.as_ref().is_some_and(|l| l.connected),
                    link: link_word,
                    channel: channel.id(),
                    channel_name: channel.name(),
                    picture: self.picture_of(&channel),
                    shared_with: channel.follower_ids().into_iter().filter(|d| *d != cfg.device).collect(),
                    fading: p.fading(),
                    device: cfg.device,
                }
            })
            .collect()
    }

    /// Every channel, Channel 1 first.
    #[must_use]
    pub fn channel_summaries(&self) -> Vec<ChannelSummary> {
        self.channels()
            .iter()
            .map(|c| ChannelSummary {
                id: c.id(),
                name: c.name(),
                home: c.id() == HOME_CHANNEL,
                picture: self.picture_of(c),
                output: self.output(),
                panels: c.follower_ids(),
            })
            .collect()
    }

    /// Test only: a collection whose channels never start a render thread.
    #[cfg(test)]
    fn quiet(memory: SharedMemory) -> Self {
        let panels = Panels {
            inner: Mutex::new(Inner { panels: Vec::new(), channels: BTreeMap::new(), next: HOME_CHANNEL + 1, retired: Vec::new() }),
            memory,
            meter: Arc::new(SocketMeter::default()),
            output: shared_output(state::default_output()),
            faults: true,
            autostart: false,
        };
        panels.ensure_home();
        panels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Working;

    fn def(id: &str) -> &'static PatchDef {
        find_patch(id, true).expect("a patch this build has")
    }

    fn channel_of(p: &Arc<Panel>) -> Arc<Channel> {
        p.channel().expect("on a channel")
    }

    /// Two panels, both adopted: both on Channel 1.
    fn two(panels: &Panels) -> (Arc<Panel>, Arc<Panel>) {
        panels.adopt("a");
        panels.adopt("b");
        (panels.get("a").expect("a"), panels.get("b").expect("b"))
    }

    /// **There is always Channel 1**, and **a new panel joins it**: on, and
    /// fading in from black before it is on the shared bytes.
    #[test]
    fn a_new_panel_joins_channel_1() {
        let panels = Panels::quiet(SharedMemory::default());
        assert_eq!(panels.channels().len(), 1, "a studio with no panel has Channel 1");
        assert_eq!(panels.home().id(), HOME_CHANNEL);
        assert_eq!(panels.home().name(), "Channel 1");
        assert!(panels.adopt("4a00a4"));
        assert!(!panels.adopt("4a00a4"), "once");
        let p = panels.get("4a00a4").expect("adopted");
        assert_eq!(channel_of(&p).id(), HOME_CHANNEL);
        assert!(p.cfg().on, "and lit");
        assert!(p.fading(), "fading in from black");
        assert_eq!(panels.home().follower_ids(), vec!["4a00a4".to_string()]);
    }

    /// **New, rename, delete.** A new channel is a copy of the one it came
    /// from (Channel 1 by default) and has no panels; a name is checked;
    /// deleting moves its panels to Channel 1; Channel 1 cannot go.
    #[test]
    fn channels_are_made_renamed_and_deleted() {
        let panels = Panels::quiet(SharedMemory::default());
        let home = panels.home();
        panels.pick(&home, def("clocks-dials"), "", None).expect("dials");
        home.edit(&crate::channel::Edit { seed: Some(77), ..Default::default() }).expect("seed");
        panels.set_output(Output { panel_model: false, ..Output::default() });

        let second = panels.new_channel(None, None).expect("a new channel");
        assert_eq!((second.id(), second.name().as_str()), (2, "Channel 2"));
        let s = second.stored();
        assert_eq!((s.patch.as_str(), s.seed), ("clocks-dials", 77), "a copy of Channel 1");
        assert!(!second.output().panel_model && !home.output().panel_model, "the output is the studio's, one for both");
        assert!(second.follower_ids().is_empty(), "with no panels");

        let kitchen = panels.new_channel(Some("  Kitchen "), Some(&second)).expect("named");
        assert_eq!((kitchen.id(), kitchen.name().as_str()), (3, "Kitchen"));
        assert!(panels.new_channel(Some(&"x".repeat(41)), None).is_err(), "too long");
        assert!(panels.rename_channel(&kitchen, "   ").is_err(), "a name is needed");
        panels.rename_channel(&kitchen, "Hall").expect("rename");
        assert_eq!(kitchen.name(), "Hall");

        let (a, b) = two(&panels);
        panels.move_panel(&a, &kitchen, None);
        panels.move_panel(&b, &kitchen, None);
        assert_eq!(kitchen.follower_ids(), vec!["a".to_string(), "b".to_string()]);
        assert!(panels.delete_channel(&panels.home()).is_err(), "Channel 1 stays");
        panels.delete_channel(&kitchen).expect("delete");
        assert!(panels.channel(3).is_none(), "gone");
        assert_eq!(channel_of(&a).id(), HOME_CHANNEL, "its panels are on Channel 1");
        assert_eq!(channel_of(&b).id(), HOME_CHANNEL);
        assert!(a.fading(), "and fade there");
        let again = panels.new_channel(None, None).expect("another");
        assert_eq!(again.id(), 4, "ids are never reused");
    }

    /// **Picking a picture changes the channel** - every panel on it. There
    /// is no join, split or detach any more.
    #[test]
    fn picking_changes_the_channel_for_every_panel_on_it() {
        let panels = Panels::quiet(SharedMemory::default());
        let (a, b) = two(&panels);
        panels.pick(&channel_of(&a), def("metaballs"), "", None).expect("pick");
        assert!(Arc::ptr_eq(&channel_of(&a), &channel_of(&b)), "still one channel");
        assert_eq!(channel_of(&b).patch(), "metaballs", "b changed with it");
        assert_eq!(panels.channels().len(), 1, "no channel was made by a pick");
    }

    /// A named setting is a picture too.
    #[test]
    fn a_named_setting_is_a_picture() {
        let memory = SharedMemory::default();
        let lava = Working { params: BTreeMap::from([("size".to_string(), 2.5)]), seed: 7, speed: 1.0 };
        memory.save_setting(def("metaballs"), "Lava", &lava).expect("save");
        let panels = Panels::quiet(memory);
        let home = panels.home();
        panels.pick(&home, def("metaballs"), "Lava", None).expect("Lava");
        assert!(panels.pick(&home, def("metaballs"), "lava ", None).is_err(), "names are exact after trimming");
        let s = panels.state_of(&home, None);
        assert_eq!((s.setting.as_str(), s.seed, s.params["size"], s.modified), ("Lava", 7, 2.5, false));
    }

    /// Moving a panel: it fades, alone; the other stays; moving back is on
    /// the shared channel again.
    #[test]
    fn moving_a_panel_moves_that_panel_only() {
        let panels = Panels::quiet(SharedMemory::default());
        let (a, b) = two(&panels);
        let other = panels.new_channel(Some("Other"), None).expect("new");
        panels.move_panel(&b, &other, None);
        assert!(Arc::ptr_eq(&channel_of(&b), &other));
        assert_eq!(channel_of(&a).id(), HOME_CHANNEL, "a stays");
        let s = panels.state_of(&other, Some(&b));
        assert_eq!((s.channel, s.channel_name.as_str(), s.device.as_str()), (other.id(), "Other", "b"));
        assert_eq!(s.panels, vec!["b".to_string()]);
        panels.move_panel(&b, &panels.home(), None);
        assert_eq!(panels.home().follower_ids(), vec!["a".to_string(), "b".to_string()]);
    }

    /// A setting renamed or deleted follows every channel on it.
    #[test]
    fn a_setting_renamed_or_deleted_follows_every_channel_on_it() {
        let memory = SharedMemory::default();
        let lava = Working { params: BTreeMap::from([("size".to_string(), 2.5)]), seed: 7, speed: 1.0 };
        memory.save_setting(def("metaballs"), "Lava", &lava).expect("save");
        let panels = Panels::quiet(memory.clone());
        let home = panels.home();
        panels.pick(&home, def("metaballs"), "Lava", None).expect("Lava");
        memory.rename_setting(def("metaballs"), "Lava", "Lava lamp").expect("rename");
        panels.setting_renamed("metaballs", "Lava", "Lava lamp");
        assert_eq!(panels.state_of(&home, None).setting, "Lava lamp");
        memory.delete_setting(def("metaballs"), "Lava lamp").expect("delete");
        panels.setting_deleted("metaballs", "Lava lamp");
        let s = panels.state_of(&home, None);
        assert_eq!((s.setting.as_str(), s.modified), ("Default", true), "on Default, and honestly modified");
    }

    /// Run every channel's render thread for a while - the given panels'
    /// fades over - then stop them and wait for them to finish.
    fn render_a_while(panels: &Panels, ticks: u64) {
        panels.start();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while panels.channels().iter().filter(|c| !c.follower_ids().is_empty()).any(|c| c.ticks() < ticks) {
            assert!(std::time::Instant::now() < deadline, "the channels never rendered");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for c in panels.channels() {
            c.shutdown();
        }
        let mut last: Vec<u64> = Vec::new();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let now: Vec<u64> = panels.channels().iter().map(|c| c.ticks()).collect();
            if now == last {
                break;
            }
            last = now;
        }
    }

    /// **Encode once**: a channel with two panels encodes exactly once per
    /// tick - not once per panel - and each panel is handed every tick.
    #[test]
    fn a_channel_encodes_once_per_tick_whoever_is_on_it() {
        let panels = Panels::quiet(SharedMemory::default());
        let (a, b) = two(&panels);
        let home = panels.home();
        render_a_while(&panels, 90);
        let ticks = home.ticks();
        assert!(ticks >= 90);
        assert_eq!(home.encodes(), ticks, "one encode per tick, for two panels");
        assert_eq!((a.presents(), b.presents()), (ticks, ticks), "every tick handed to both");
        assert!(!a.fading() && !b.fading(), "their fades in are over");
    }

    /// An empty channel nobody watches does not render; one that is watched
    /// does.
    #[test]
    fn an_empty_channel_renders_only_while_watched() {
        let panels = Panels::quiet(SharedMemory::default());
        let home = panels.home();
        home.ensure_running();
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(home.ticks(), 0, "no panel, nobody watching: parked");
        let mut viewer = home.screen().viewer();
        viewer.set_wants_frames(true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while home.ticks() < 5 {
            assert!(std::time::Instant::now() < deadline, "a watched channel never rendered");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        home.shutdown();
    }

    /// The file round-trips: panels in their order, each on its channel, and
    /// an empty channel kept.
    #[test]
    fn what_is_stored_loads_back_the_same() {
        let panels = Panels::quiet(SharedMemory::default());
        let (_a, b) = two(&panels);
        panels.adopt("c");
        let other = panels.new_channel(Some("Other"), None).expect("new");
        panels.pick(&other, def("clocks-dials"), "", None).expect("dials");
        panels.move_panel(&b, &other, None);
        panels.new_channel(Some("Empty"), None).expect("empty");
        let (sp, sc) = panels.stored();
        assert_eq!(sc.len(), 3, "the empty channel is kept");
        let again = Panels::quiet(SharedMemory::default());
        again.load(sp.clone(), sc.clone());
        assert_eq!(again.stored(), (sp, sc));
        assert_eq!(again.ids(), vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(again.get("b").and_then(|p| p.channel()).map(|c| c.id()), Some(2));
        assert_eq!(again.new_channel(None, None).expect("next").id(), 4, "the next id carries on");
    }
}
