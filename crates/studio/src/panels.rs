//! Every panel and every channel, and the rules that tie them (card 350).
//!
//! **Several panels: channels** (`docs/design/studio-vision.md`, normative).
//! The owner, 2026-09-29: *"the same patch can be sent to one or more devices,
//! and more than one patch can be active at a time"*. A [`Channel`] renders a
//! picture once; a [`Panel`] follows one channel, or none (idle). This type
//! owns both collections and is the only thing that changes which panel
//! follows which channel, so the topology has one lock and one set of rules:
//!
//! **Picking a picture for a panel** ([`Panels::pick`]) - the patch list, a
//! named setting, Home Assistant - resolves to a channel by three rules, in
//! order:
//!
//! 1. another channel already shows exactly that picture (patch + named
//!    setting, no unsaved tweaks): the panel **joins** it;
//! 2. the panel is alone on its channel: that channel **changes** picture,
//!    with the 2 s fade it has always had;
//! 3. otherwise the panel **leaves** its group for a new channel showing the
//!    picture.
//!
//! **Same as** ([`Panels::same_as`]) joins another panel's channel, tweaks and
//! all; **Detach** ([`Panels::detach`]) gives a panel a copy of its channel,
//! with its own clock from then on. A panel that moves between channels fades
//! over [`FADE_MANUAL`] in its own output stage; a panel joining a channel
//! follows that channel's clock, it does not restart it.
//!
//! **Panels are adopted, idle** ([`Panels::adopt`]): every device the registry
//! knows becomes a panel with no channel and no stream. A studio that has
//! found no panel at all still has a picture, on the **unbound** stand-in
//! ([`UNBOUND`]), which exists only while there is no real panel: the first
//! panel somebody *names* (`set_panel {"on":true,"to":...}`, `devices/add`
//! with `play`) is renamed onto it, so the picture the page was showing simply
//! starts reaching the panel.
//!
//! **The first panel** is the one a route or a socket without `panel` means:
//! panels are kept in the order they were adopted.
//!
//! Lock order, everywhere: this type's lock, then a panel's channel slot, then
//! a channel's follower list, then a panel's output stage. A render thread
//! only ever takes the last two, and never while holding a follower list.

use screeny_art::output::PanelStatus;
use screeny_art::patch::{Params, PatchDef, Playing};
use screeny_art::Output;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::channel::{fade_len, find_patch, Channel, ChannelId, FADE_MANUAL};
use crate::devices::Registry;
use crate::page::{SocketMeter, StudioState};
use crate::panel::Panel;
use crate::state::{self, SharedMemory, StoredChannel, StoredPanel, DEFAULT_SETTING, UNBOUND};

/// What `/api/v1/status` says about one panel and the picture it shows.
///
/// Card 106's per-device `player`, kept in shape so that scripts and tests
/// written against it keep working, and made of two halves since card 350: the
/// picture's (the channel's) and the device's (the panel's). A panel on a
/// shared channel reports the shared picture - the same `ticks` as the other
/// panels on it, because it is the same render.
#[derive(Clone, Debug, Serialize)]
pub struct PlayerStatus {
    /// The device id, or empty for the unbound stand-in.
    pub device: String,
    /// Panel output: false means the link is released and the panel is on its
    /// own idle screen.
    pub on: bool,
    /// Empty while the panel is idle.
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
    pub output: Output,
    /// True when its channel's render thread is alive and ticking. False for
    /// an idle panel, which has nothing to render.
    pub running: bool,
    /// True when this is the first panel: the one a route without `panel`
    /// means, and what the page shows before card 351 lets it choose.
    pub focused: bool,
    /// The rate its channel's render loop is actually achieving.
    pub fps_measured: f32,
    /// What a composing patch says it is performing.
    pub playing: Option<Playing>,
    pub health: PlayerHealth,
    /// The link, or `None` when there is none (output off, or idle).
    pub panel: Option<PanelStatus>,
    /// Card 350: the channel it follows, `null` when idle.
    pub channel: Option<ChannelId>,
    /// The named setting the picture came from.
    pub setting: String,
    /// Whether the picture has moved away from that setting.
    pub modified: bool,
    /// The other panels on the same channel.
    pub shared_with: Vec<String>,
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

/// One card in the overview (card 350): what the page draws a panel's card
/// from, and what `GET /api/v1/panels` and the socket's `panels` message
/// carry.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PanelSummary {
    /// The device id: what every `panel` parameter takes. Empty for the
    /// unbound stand-in.
    pub device: String,
    /// What to call it: the name set here, else the device's own, else its
    /// instance name, else the id.
    pub name: String,
    /// True for the stand-in of a studio that has found no panel yet.
    pub unbound: bool,
    /// True for the first panel: what a route without `panel` means.
    pub first: bool,
    /// Panel output.
    pub on: bool,
    /// The brightness policy, 0-255, or `null` for "whatever the device has".
    pub brightness: Option<u8>,
    /// `idle` (no picture), `off` (output switched off), `none` (the unbound
    /// stand-in, which has no device), or the link's own state: `up`,
    /// `connecting`, `waiting`, `closed`.
    pub link: String,
    /// True only while the link is `up`.
    pub connected: bool,
    /// The channel it follows; `null` when idle.
    pub channel: Option<ChannelId>,
    /// What it shows; `null` when idle.
    pub picture: Option<PictureSummary>,
    /// The other panels on the same channel, by device id.
    pub shared_with: Vec<String>,
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
    /// In the order they were adopted: the first is the one a route without
    /// `panel` means.
    panels: Vec<Arc<Panel>>,
    channels: BTreeMap<ChannelId, Arc<Channel>>,
    next: ChannelId,
}

/// Every panel and every channel.
pub struct Panels {
    inner: Mutex<Inner>,
    memory: SharedMemory,
    meter: Arc<SocketMeter>,
    faults: bool,
    /// Start a channel's render thread as soon as it is made. Off only in the
    /// unit tests that look at the topology and nothing else.
    autostart: bool,
}

impl Panels {
    #[must_use]
    pub fn new(memory: SharedMemory, faults: bool) -> Self {
        Panels {
            inner: Mutex::new(Inner { panels: Vec::new(), channels: BTreeMap::new(), next: 1 }),
            memory,
            meter: Arc::new(SocketMeter::default()),
            faults,
            autostart: true,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The studio's one library of named settings, shared by every channel.
    #[must_use]
    pub fn memory(&self) -> SharedMemory {
        self.memory.clone()
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

    /// The first panel, if there is any: what a route without `panel` means.
    #[must_use]
    pub fn first(&self) -> Option<Arc<Panel>> {
        self.lock().panels.first().cloned()
    }

    #[must_use]
    pub fn get(&self, device: &str) -> Option<Arc<Panel>> {
        self.lock().panels.iter().find(|p| p.device() == device).cloned()
    }

    /// Every panel, first first.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<Panel>> {
        self.lock().panels.clone()
    }

    /// Their device ids, in the same order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.lock().panels.iter().map(|p| p.device()).collect()
    }

    /// Every channel, by id.
    #[must_use]
    pub fn channels(&self) -> Vec<Arc<Channel>> {
        self.lock().channels.values().cloned().collect()
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

    /// Adopt what the state file had. Render threads start with
    /// [`Panels::start`].
    pub fn load(&self, panels: Vec<StoredPanel>, channels: Vec<StoredChannel>) {
        let mut inner = self.lock();
        for c in channels {
            inner.next = inner.next.max(c.id.saturating_add(1));
            inner.channels.insert(c.id, Channel::new(c, self.faults));
        }
        for sp in panels {
            if inner.panels.iter().any(|p| p.device() == sp.device) {
                continue;
            }
            let panel = Panel::new(&sp, &self.meter);
            if let Some(c) = sp.channel.and_then(|id| inner.channels.get(&id).cloned()) {
                panel.switch_channel(Some(c), f64::from(FADE_MANUAL));
            }
            inner.panels.push(panel);
        }
        Self::drop_unused_locked(&mut inner);
    }

    /// Start every channel's render thread that is not running.
    pub fn start(&self) {
        for c in self.channels() {
            c.ensure_running();
        }
    }

    /// Make sure there is a first panel. With no panel at all, that is the
    /// unbound stand-in, on a picture of its own: a studio always has a
    /// picture, even before it has a panel.
    pub fn ensure_first(&self) -> Arc<Panel> {
        let mut inner = self.lock();
        if let Some(first) = inner.panels.first() {
            return Arc::clone(first);
        }
        let panel = Panel::new(&StoredPanel { device: UNBOUND.to_string(), ..StoredPanel::default() }, &self.meter);
        let channel = self.new_channel_locked(&mut inner, StoredChannel::default());
        panel.switch_channel(Some(channel), f64::from(FADE_MANUAL));
        inner.panels.push(Arc::clone(&panel));
        panel
    }

    /// **Adopt a device as a panel, idle**: no channel, no stream, so the
    /// device shows its own screen until somebody gives it a picture. The
    /// unbound stand-in goes the moment there is a real panel. True when the
    /// panel is new.
    pub fn adopt(&self, device: &str) -> bool {
        if device == UNBOUND {
            return false;
        }
        let mut inner = self.lock();
        if inner.panels.iter().any(|p| p.device() == device) {
            return false;
        }
        inner.panels.push(Panel::new(&StoredPanel { device: device.to_string(), ..StoredPanel::default() }, &self.meter));
        if let Some(i) = inner.panels.iter().position(|p| p.is_unbound()) {
            let stand_in = inner.panels.remove(i);
            stand_in.drop_channel();
            stand_in.shutdown();
        }
        Self::drop_unused_locked(&mut inner);
        true
    }

    /// **The panel for a device somebody has named** - to drive it, or to
    /// configure it. If there is no panel for it yet and the studio is still on
    /// its unbound stand-in, the stand-in is renamed onto the device: the
    /// picture the page was showing carries straight on to the panel instead
    /// of restarting on it. Otherwise it is adopted, idle.
    pub fn attach(&self, device: &str) -> Arc<Panel> {
        let mut inner = self.lock();
        if let Some(p) = inner.panels.iter().find(|p| p.device() == device) {
            return Arc::clone(p);
        }
        if let Some(stand_in) = inner.panels.iter().find(|p| p.is_unbound()) {
            stand_in.rename(device);
            return Arc::clone(stand_in);
        }
        let panel = Panel::new(&StoredPanel { device: device.to_string(), ..StoredPanel::default() }, &self.meter);
        inner.panels.push(Arc::clone(&panel));
        panel
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
        Self::drop_unused_locked(&mut inner);
    }

    /// Forget a panel: its link is released and its channel goes with it if
    /// nobody else is on it.
    pub fn remove(&self, device: &str) {
        let mut inner = self.lock();
        if let Some(i) = inner.panels.iter().position(|p| p.device() == device) {
            let p = inner.panels.remove(i);
            p.drop_channel();
            p.shutdown();
        }
        Self::drop_unused_locked(&mut inner);
    }

    /// Drop every channel nothing needs any more (card 350: *"channels with no
    /// panels are dropped"*). A channel a panel is still fading away from is
    /// kept until the fade is over.
    pub fn drop_unused(&self) {
        Self::drop_unused_locked(&mut self.lock());
    }

    fn drop_unused_locked(inner: &mut Inner) {
        let gone: Vec<ChannelId> = inner.channels.iter().filter(|(_, c)| c.unused()).map(|(id, _)| *id).collect();
        for id in gone {
            if let Some(c) = inner.channels.remove(&id) {
                c.shutdown();
            }
        }
    }

    fn new_channel_locked(&self, inner: &mut Inner, mut stored: StoredChannel) -> Arc<Channel> {
        stored.id = inner.next;
        inner.next = inner.next.saturating_add(1);
        let c = Channel::new(stored, self.faults);
        inner.channels.insert(c.id(), Arc::clone(&c));
        if self.autostart {
            c.ensure_running();
        }
        c
    }

    /// Move a panel onto `next`, fading, and drop whatever that leaves unused.
    fn follow_locked(inner: &mut Inner, panel: &Arc<Panel>, next: Option<Arc<Channel>>, fade: Option<f32>) {
        panel.switch_channel(next, fade_len(fade));
        Self::drop_unused_locked(inner);
    }

    // --------------------------------------------------------- the rules ---

    /// **Pick a picture for a panel**: `def` on the setting named `setting`
    /// (empty or `Default` is Default), by card 350's three rules - join a
    /// channel already showing exactly that, else change the panel's own
    /// channel if it is alone on it, else split off onto a new one.
    ///
    /// `exclusive` is for a request that goes on to *edit* the picture (a seed,
    /// a parameter): it must not join somebody else's channel, or the edit
    /// would land on them too, so rule 1 is skipped and a shared channel is
    /// left rather than changed.
    ///
    /// Returns the channel the panel is on afterwards.
    ///
    /// # Errors
    ///
    /// If the patch has no setting by that name.
    pub fn pick(
        &self,
        panel: &Arc<Panel>,
        def: &'static PatchDef,
        setting: &str,
        exclusive: bool,
        fade: Option<f32>,
    ) -> Result<Arc<Channel>, String> {
        let (work, repaired) = self.memory.usable_setting(def, setting)?;
        if !repaired.is_empty() {
            // The `repaired` voice, said once per load rather than per value:
            // a setting older than the patch is what this is for.
            eprintln!("studio: `{}` loading `{}`: {}", def.id, setting.trim(), repaired.join("; "));
        }
        let mut inner = self.lock();
        let current = panel.channel();
        let alone = current.as_ref().is_some_and(|c| c.followers().len() <= 1);
        // Already showing exactly that: nothing to do.
        if let Some(c) = &current {
            if c.shows(def.id, setting, &self.memory) && (alone || !exclusive) {
                return Ok(Arc::clone(c));
            }
        }
        // Rule 1: join a channel that already shows it.
        if !exclusive {
            let other = inner
                .channels
                .values()
                .find(|c| current.as_ref().is_none_or(|cur| !Arc::ptr_eq(cur, c)) && !c.unused() && c.shows(def.id, setting, &self.memory))
                .cloned();
            if let Some(other) = other {
                Self::follow_locked(&mut inner, panel, Some(Arc::clone(&other)), fade);
                return Ok(other);
            }
        }
        // Rule 2: alone on its channel, so the channel changes.
        if let Some(c) = current.filter(|_| alone) {
            c.show(def, setting, work, fade);
            return Ok(c);
        }
        // Rule 3: a channel of its own.
        let stored = StoredChannel { id: 0, patch: def.id.to_string(), setting: state::setting_key(setting), seed: work.seed, params: work.params };
        let c = self.new_channel_locked(&mut inner, stored);
        Self::follow_locked(&mut inner, panel, Some(Arc::clone(&c)), fade);
        Ok(c)
    }

    /// **"Same as"**: put `panel` on `other`'s channel, tweaks and all - the
    /// way to share a picture nobody has saved as a setting. It fades there,
    /// and follows that channel's clock from then on.
    ///
    /// # Errors
    ///
    /// If `other` is idle: it has no picture to share.
    pub fn same_as(&self, panel: &Arc<Panel>, other: &Arc<Panel>) -> Result<Arc<Channel>, String> {
        let mut inner = self.lock();
        let Some(target) = other.channel() else {
            return Err(format!("`{}` is idle: it has no picture to share.", other.device()));
        };
        if panel.channel().is_some_and(|c| Arc::ptr_eq(&c, &target)) {
            return Ok(target);
        }
        Self::follow_locked(&mut inner, panel, Some(Arc::clone(&target)), None);
        Ok(target)
    }

    /// **Detach**: give `panel` a copy of its channel - same patch, setting and
    /// working copy, its own clock from then on - so an edit to it no longer
    /// reaches the others. A panel already alone on its channel is left as it
    /// is.
    ///
    /// # Errors
    ///
    /// If the panel is idle.
    pub fn detach(&self, panel: &Arc<Panel>) -> Result<Arc<Channel>, String> {
        let mut inner = self.lock();
        let Some(current) = panel.channel() else {
            return Err(format!("`{}` is idle: it has no picture to detach from.", panel.device()));
        };
        if current.followers().len() <= 1 {
            return Ok(current);
        }
        let copy = self.new_channel_locked(&mut inner, current.stored());
        Self::follow_locked(&mut inner, panel, Some(Arc::clone(&copy)), None);
        Ok(copy)
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
        for c in inner.channels.values() {
            c.shutdown();
        }
        for p in &inner.panels {
            p.shutdown();
        }
    }

    // ------------------------------------------------------------ views ---

    /// What the page draws itself from, for one panel.
    #[must_use]
    pub fn state_of(&self, panel: &Arc<Panel>) -> StudioState {
        let cfg = panel.cfg();
        let channel = panel.channel();
        let base = StudioState {
            patch: String::new(),
            seed: 0,
            params: BTreeMap::new(),
            output: cfg.output,
            paused: false,
            speed: 1.0,
            fps: screeny_art::FPS,
            on: cfg.on,
            device: cfg.device.clone(),
            setting: DEFAULT_SETTING.to_string(),
            settings: Vec::new(),
            modified: false,
            channel: None,
            shared_with: Vec::new(),
        };
        let Some(channel) = channel else { return base };
        let stored = channel.stored();
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
            setting: if stored.setting.is_empty() { DEFAULT_SETTING.to_string() } else { stored.setting },
            settings,
            modified,
            channel: Some(channel.id()),
            shared_with: channel.follower_ids().into_iter().filter(|d| *d != cfg.device).collect(),
            ..base
        }
    }

    /// Everything `/api/v1/status` shows about one panel.
    #[must_use]
    pub fn status_of(&self, panel: &Arc<Panel>) -> PlayerStatus {
        let cfg = panel.cfg();
        let link = panel.link_health();
        let channel = panel.channel().map(|c| (c.status(), c.follower_ids(), c.modified(&self.memory)));
        let mut health = PlayerHealth {
            last_frame_ago: link.last_frame_ago,
            sessions: link.sessions,
            link_ups: link.link_ups,
            reconnects: link.reconnects,
            brightness_applied: link.brightness_applied,
            brightness_cap: link.brightness_cap,
            last_error: link.last_error.clone(),
            ..PlayerHealth::default()
        };
        let mut out = PlayerStatus {
            device: cfg.device.clone(),
            on: cfg.on,
            patch: String::new(),
            patch_name: String::new(),
            seed: 0,
            params: BTreeMap::new(),
            fps: screeny_art::FPS,
            paused: false,
            speed: 1.0,
            brightness: cfg.brightness,
            output: cfg.output,
            running: false,
            focused: self.is_first(panel),
            fps_measured: 0.0,
            playing: None,
            health: PlayerHealth::default(),
            panel: panel.link_status(),
            channel: None,
            setting: DEFAULT_SETTING.to_string(),
            modified: false,
            shared_with: Vec::new(),
        };
        if let Some((s, followers, modified)) = channel {
            let h = s.health;
            health.ticks = s.ticks;
            health.panics = h.panics;
            health.stalls = h.stalls;
            health.restarts = h.restarts;
            health.abandoned = h.abandoned;
            health.fell_back_from = h.fell_back_from;
            health.refused = h.refused;
            health.gave_up = h.gave_up;
            health.last_tick_ago = s.last_tick_ago;
            if health.last_error.is_none() {
                health.last_error = h.last_error;
            }
            out.patch_name = find_patch(&s.stored.patch, self.faults).map_or("", |d| d.name).to_string();
            out.patch = s.stored.patch;
            out.seed = s.stored.seed;
            out.params = s.stored.params;
            out.setting = if s.stored.setting.is_empty() { DEFAULT_SETTING.to_string() } else { s.stored.setting };
            out.modified = modified;
            out.running = s.running;
            out.fps_measured = s.fps_measured;
            out.playing = s.playing;
            out.channel = Some(s.id);
            out.shared_with = followers.into_iter().filter(|d| *d != cfg.device).collect();
        }
        out.health = health;
        out
    }

    /// The overview: one [`PanelSummary`] per panel, first first.
    #[must_use]
    pub fn summaries(&self, devices: &Registry) -> Vec<PanelSummary> {
        let all = self.all();
        all.iter()
            .enumerate()
            .map(|(i, p)| {
                let cfg = p.cfg();
                let unbound = cfg.device == UNBOUND;
                let channel = p.channel();
                let link = p.link_status();
                let link_word = match (&channel, &link) {
                    _ if unbound => "none".to_string(),
                    (None, _) => "idle".to_string(),
                    _ if !cfg.on => "off".to_string(),
                    (Some(_), Some(l)) => l.state.to_string(),
                    (Some(_), None) => "connecting".to_string(),
                };
                let picture = channel.as_ref().map(|c| {
                    let s = c.stored();
                    PictureSummary {
                        patch_name: find_patch(&s.patch, self.faults).map_or("", |d| d.name).to_string(),
                        patch: s.patch,
                        setting: if s.setting.is_empty() { DEFAULT_SETTING.to_string() } else { s.setting },
                        modified: c.modified(&self.memory),
                    }
                });
                let name = if unbound {
                    "No panel yet".to_string()
                } else {
                    devices.get(&cfg.device).map_or_else(|| cfg.device.clone(), |d| d.label())
                };
                PanelSummary {
                    name,
                    unbound,
                    first: i == 0,
                    on: cfg.on,
                    brightness: cfg.brightness,
                    connected: link.as_ref().is_some_and(|l| l.connected),
                    link: link_word,
                    channel: channel.as_ref().map(|c| c.id()),
                    picture,
                    shared_with: channel
                        .as_ref()
                        .map(|c| c.follower_ids().into_iter().filter(|d| *d != cfg.device).collect())
                        .unwrap_or_default(),
                    device: cfg.device,
                }
            })
            .collect()
    }

    /// Test only: a collection whose channels never start a render thread.
    #[cfg(test)]
    fn quiet(memory: SharedMemory) -> Self {
        Panels { autostart: false, ..Panels::new(memory, true) }
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
        p.channel().expect("not idle")
    }

    /// Two panels on one channel, the way a studio gets there: both adopted
    /// idle, both asked for the same picture.
    fn two_on_one(panels: &Panels) -> (Arc<Panel>, Arc<Panel>) {
        panels.adopt("a");
        panels.adopt("b");
        let a = panels.get("a").expect("a");
        let b = panels.get("b").expect("b");
        panels.pick(&a, def("metaballs"), "", false, None).expect("a picks");
        panels.pick(&b, def("metaballs"), "", false, None).expect("b picks");
        (a, b)
    }

    /// **Auto-adopt**: a device becomes a panel that is idle - no channel, so
    /// no link and no stream - and the unbound stand-in goes when the first
    /// real one arrives.
    #[test]
    fn adopting_makes_an_idle_panel_and_retires_the_stand_in() {
        let panels = Panels::quiet(SharedMemory::default());
        let stand_in = panels.ensure_first();
        assert!(stand_in.is_unbound());
        assert!(stand_in.channel().is_some(), "a studio always has a picture");
        assert_eq!(panels.channels().len(), 1);

        assert!(panels.adopt("4a00a4"));
        assert!(!panels.adopt("4a00a4"), "once");
        let p = panels.get("4a00a4").expect("adopted");
        assert!(p.channel().is_none(), "a new panel is idle");
        assert!(p.cfg().on, "and will stream the moment it has a picture");
        assert_eq!(panels.ids(), vec!["4a00a4".to_string()], "the stand-in is gone");
        assert!(panels.channels().is_empty(), "and its channel with it: a channel with no panel is dropped");
    }

    /// **Rule 1**: two panels asking for the same unmodified picture end up on
    /// one channel - in sync without anybody saying "mirror".
    #[test]
    fn the_same_picture_joins_the_channel_showing_it() {
        let panels = Panels::quiet(SharedMemory::default());
        let (a, b) = two_on_one(&panels);
        assert!(Arc::ptr_eq(&channel_of(&a), &channel_of(&b)), "one channel for one picture");
        assert_eq!(panels.channels().len(), 1);
        assert_eq!(panels.state_of(&a).shared_with, vec!["b".to_string()]);
        assert_eq!(panels.state_of(&b).shared_with, vec!["a".to_string()]);
    }

    /// Rule 1 is for **unmodified** pictures only: a channel somebody has
    /// tweaked is not "that picture" any more.
    #[test]
    fn a_tweaked_picture_is_not_joined() {
        let panels = Panels::quiet(SharedMemory::default());
        panels.adopt("a");
        panels.adopt("b");
        let a = panels.get("a").expect("a");
        let b = panels.get("b").expect("b");
        panels.pick(&a, def("metaballs"), "", false, None).expect("a");
        channel_of(&a).edit(&crate::channel::Edit { param: Some(("size".into(), 2.5)), ..Default::default() }).expect("size");
        panels.pick(&b, def("metaballs"), "", false, None).expect("b");
        assert!(!Arc::ptr_eq(&channel_of(&a), &channel_of(&b)));
    }

    /// **Rule 2**: a panel alone on its channel changes that channel - the
    /// same channel, so the patch change is the channel's own 2 s fade.
    #[test]
    fn a_panel_alone_changes_its_own_channel() {
        let panels = Panels::quiet(SharedMemory::default());
        panels.adopt("a");
        let a = panels.get("a").expect("a");
        let first = panels.pick(&a, def("metaballs"), "", false, None).expect("metaballs");
        let second = panels.pick(&a, def("clocks-dials"), "", false, None).expect("dials");
        assert!(Arc::ptr_eq(&first, &second), "the same channel, changed");
        assert_eq!(second.patch(), "clocks-dials");
        assert_eq!(panels.channels().len(), 1);
    }

    /// **Rule 3**: a panel sharing its channel that asks for something else
    /// leaves the group for a channel of its own; the other stays put.
    #[test]
    fn a_panel_in_a_group_splits_off() {
        let panels = Panels::quiet(SharedMemory::default());
        let (a, b) = two_on_one(&panels);
        let shared = channel_of(&a);
        panels.pick(&b, def("clocks-dials"), "", false, None).expect("dials");
        assert!(Arc::ptr_eq(&channel_of(&a), &shared), "a is where it was");
        assert_eq!(shared.patch(), "metaballs", "and still showing what it was");
        assert_eq!(channel_of(&b).patch(), "clocks-dials");
        assert_eq!(panels.channels().len(), 2);
    }

    /// A request that goes on to edit the picture must not join another
    /// panel's channel, or its edit would reach them too.
    #[test]
    fn an_exclusive_pick_never_shares() {
        let panels = Panels::quiet(SharedMemory::default());
        let (a, b) = two_on_one(&panels);
        panels.pick(&b, def("metaballs"), "", true, None).expect("exclusive");
        assert!(!Arc::ptr_eq(&channel_of(&a), &channel_of(&b)), "b has a metaballs of its own");
    }

    /// A named setting is a picture too: the three rules apply to loading one.
    #[test]
    fn a_named_setting_is_a_picture() {
        let memory = SharedMemory::default();
        let lava = Working { params: BTreeMap::from([("size".to_string(), 2.5)]), seed: 7, speed: 1.0 };
        memory.save_setting(def("metaballs"), "Lava", &lava).expect("save");
        let panels = Panels::quiet(memory);
        panels.adopt("a");
        panels.adopt("b");
        let a = panels.get("a").expect("a");
        let b = panels.get("b").expect("b");
        panels.pick(&a, def("metaballs"), "Lava", false, None).expect("a");
        panels.pick(&b, def("metaballs"), "", false, None).expect("b on Default");
        assert!(!Arc::ptr_eq(&channel_of(&a), &channel_of(&b)), "Default is not Lava");
        assert!(panels.pick(&b, def("metaballs"), "lava ", false, None).is_err(), "names are exact after trimming");
        panels.pick(&b, def("metaballs"), " Lava", false, None).expect("b on Lava");
        assert!(Arc::ptr_eq(&channel_of(&a), &channel_of(&b)), "Lava is Lava");
        let s = panels.state_of(&b);
        assert_eq!((s.setting.as_str(), s.seed, s.params["size"], s.modified), ("Lava", 7, 2.5, false));
    }

    /// **Same as** shares a picture nobody has saved - tweaks and all.
    #[test]
    fn same_as_joins_another_panels_channel_tweaks_and_all() {
        let panels = Panels::quiet(SharedMemory::default());
        panels.adopt("a");
        panels.adopt("b");
        let a = panels.get("a").expect("a");
        let b = panels.get("b").expect("b");
        panels.pick(&a, def("metaballs"), "", false, None).expect("a");
        channel_of(&a).edit(&crate::channel::Edit { seed: Some(4242), ..Default::default() }).expect("seed");
        assert!(panels.same_as(&b, &a).is_ok());
        assert!(Arc::ptr_eq(&channel_of(&a), &channel_of(&b)));
        assert_eq!(panels.state_of(&b).seed, 4242);
        assert!(panels.state_of(&b).modified, "and says honestly that it is tweaked");
        let idle = panels.attach("c");
        assert!(panels.same_as(&a, &idle).is_err(), "an idle panel has nothing to share");
    }

    /// **Detach** gives a panel a copy of its channel; an edit to the copy no
    /// longer reaches the others.
    #[test]
    fn detach_gives_a_panel_a_copy_of_its_own() {
        let panels = Panels::quiet(SharedMemory::default());
        let (a, b) = two_on_one(&panels);
        channel_of(&a).edit(&crate::channel::Edit { seed: Some(99), ..Default::default() }).expect("seed");
        let copy = panels.detach(&b).expect("detach");
        assert!(!Arc::ptr_eq(&channel_of(&a), &copy));
        assert_eq!(copy.stored().seed, 99, "the same working copy");
        assert_eq!(copy.patch(), "metaballs");
        copy.edit(&crate::channel::Edit { seed: Some(5), ..Default::default() }).expect("edit the copy");
        assert_eq!(channel_of(&a).stored().seed, 99, "the edit stays on b");
        let again = panels.detach(&b).expect("alone");
        assert!(Arc::ptr_eq(&again, &copy), "a panel alone on its channel is left as it is");
    }

    /// A channel no panel follows is dropped - here, once a panel that joined
    /// another has finished fading away from its old one.
    #[test]
    fn a_channel_with_no_panels_is_dropped() {
        let panels = Panels::quiet(SharedMemory::default());
        panels.adopt("a");
        panels.adopt("b");
        let a = panels.get("a").expect("a");
        let b = panels.get("b").expect("b");
        panels.pick(&a, def("metaballs"), "", false, None).expect("a");
        panels.pick(&b, def("clocks-dials"), "", false, None).expect("b");
        assert_eq!(panels.channels().len(), 2);
        // Two seconds of frames through b's stage: its first fade, in from
        // black, is over.
        let frame = screeny_art::Frame::black();
        let settle = || {
            for _ in 0..70 {
                b.present(&frame, 1.0 / 30.0, 0.0, 30.0);
            }
        };
        settle();
        // b joins a (rule 1): its old channel has nobody on it, but b is still
        // fading away from it.
        panels.pick(&b, def("metaballs"), "", false, None).expect("b joins");
        panels.drop_unused();
        assert_eq!(panels.channels().len(), 2, "kept while b fades away from it");
        settle();
        panels.drop_unused();
        assert_eq!(panels.channels().len(), 1, "and dropped once the fade is over");
        panels.remove("a");
        panels.remove("b");
        assert!(panels.channels().is_empty(), "forgetting the last panel on a channel drops it");
    }

    /// The panel somebody names first takes over the stand-in, so the picture
    /// the page was showing carries on to it; `rekey` keeps a panel's place.
    #[test]
    fn attaching_the_first_panel_keeps_the_picture_and_the_order_holds() {
        let panels = Panels::quiet(SharedMemory::default());
        let stand_in = panels.ensure_first();
        let picture = channel_of(&stand_in);
        let p = panels.attach("pending:127.0.0.1:9");
        assert!(Arc::ptr_eq(&p, &stand_in), "the same panel, renamed");
        assert!(Arc::ptr_eq(&channel_of(&p), &picture), "on the same picture");
        panels.adopt("zz");
        panels.rekey("pending:127.0.0.1:9", "aa0001");
        assert_eq!(panels.ids(), vec!["aa0001".to_string(), "zz".to_string()], "adoption order, not id order");
        assert!(panels.is_first(&p));
    }

    /// Renaming or deleting a setting moves every channel that was on it.
    #[test]
    fn a_setting_renamed_or_deleted_follows_every_channel_on_it() {
        let memory = SharedMemory::default();
        let lava = Working { params: BTreeMap::from([("size".to_string(), 2.5)]), seed: 7, speed: 1.0 };
        memory.save_setting(def("metaballs"), "Lava", &lava).expect("save");
        let panels = Panels::quiet(memory.clone());
        panels.adopt("a");
        let a = panels.get("a").expect("a");
        panels.pick(&a, def("metaballs"), "Lava", false, None).expect("a");
        memory.rename_setting(def("metaballs"), "Lava", "Lava lamp").expect("rename");
        panels.setting_renamed("metaballs", "Lava", "Lava lamp");
        assert_eq!(panels.state_of(&a).setting, "Lava lamp");
        assert!(!panels.state_of(&a).modified);
        memory.delete_setting(def("metaballs"), "Lava lamp").expect("delete");
        panels.setting_deleted("metaballs", "Lava lamp");
        let s = panels.state_of(&a);
        assert_eq!((s.setting.as_str(), s.modified), ("Default", true), "on Default, and honestly modified");
    }

    /// The file round-trips: panels in their order, each on its channel.
    #[test]
    fn what_is_stored_loads_back_the_same() {
        let panels = Panels::quiet(SharedMemory::default());
        let (_a, b) = two_on_one(&panels);
        panels.adopt("c");
        panels.pick(&b, def("clocks-dials"), "", true, None).expect("b");
        let (sp, sc) = panels.stored();
        let again = Panels::quiet(SharedMemory::default());
        again.load(sp.clone(), sc.clone());
        assert_eq!(again.stored(), (sp, sc));
        assert_eq!(again.ids(), vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        assert!(again.get("c").expect("c").channel().is_none(), "idle stays idle");
    }
}
