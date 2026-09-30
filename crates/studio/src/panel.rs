//! Panels: a device, and everything that is about the device (card 350).
//!
//! A panel is its link, on/off, brightness, its output settings (Dithered /
//! Bit planes, the limiter - the Panel screen's), its own [`Pipeline`] and the
//! [`Channel`] it follows. Its frames are its channel's linear frames through
//! its own pipeline, so two panels on one channel can differ in output
//! settings and still move in lock-step (`docs/design/studio-vision.md`,
//! "Several panels").
//!
//! A panel does not render. Its channel's render thread calls
//! [`Panel::present`] once per frame with the one linear frame it rendered for
//! everybody; the panel mixes it (while it is fading from another channel),
//! puts it through its pipeline, fills its own preview cell, and hands it to
//! its link. A panel with no channel is **idle**: no link, no stream, and the
//! device shows its own status screen.
//!
//! **Moving between channels fades.** A panel that changes channel keeps the
//! old channel's frames for [`FADE_MANUAL`] and blends them into the new one's
//! with `crossfade::blend`, in its own output stage, in linear light before its
//! limiter - the same place a patch change is blended, one level up. A change
//! mid-fade fades from a still of what was showing, never chaining a second
//! fade onto the first. The old channel is told a panel is leaving it, so it
//! keeps rendering at the full rate and is not dropped until the fade is over.

use screeny_art::crossfade::{blend, Crossfade};
use screeny_art::output::{Output as FrameSink, PanelStatus, SenderOutput};
use screeny_art::{Frame, Output, Pipeline};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::channel::{Channel, FADE_MANUAL};
use crate::devices::Reach;
use crate::page::{self, Screen, SocketMeter};
use crate::state::{unix_now, StoredPanel, UNBOUND};

/// A brightness the caller should apply, off the render thread.
pub struct BrightnessJob {
    pub device: String,
    pub level: u8,
}

/// What a panel is set to, as persisted - less the channel it follows, which
/// is [`Panel::channel`].
#[derive(Clone, Debug, PartialEq)]
pub struct PanelCfg {
    /// The device id, or [`UNBOUND`] for the stand-in panel a studio that has
    /// found no panel yet shows its picture on.
    pub device: String,
    /// **Panel output.** False releases the link - the panel goes back to its
    /// own idle screen - and the channel keeps rendering, because the page is
    /// still showing the picture.
    pub on: bool,
    pub output: Output,
    /// Brightness policy: a fixed level to apply whenever the link comes up,
    /// or `None` to leave whatever the device has.
    pub brightness: Option<u8>,
}

/// What a panel's link has been through: the half of `health` that is about
/// the device, not the picture.
#[derive(Clone, Debug, Default)]
pub struct LinkHealth {
    /// Sessions the **current link object** has opened.
    pub sessions: u64,
    /// Every time this panel's stream has come up **since the studio
    /// started**, carried across link rebuilds.
    pub link_ups: u64,
    /// How many times the panel has come *back* (card 171).
    pub reconnects: u64,
    /// The brightness policy as actually applied by the device.
    pub brightness_applied: Option<u8>,
    /// The device's own ceiling, learned by asking for more than it gives.
    pub brightness_cap: Option<u8>,
    /// The last thing that went wrong with the link, if anything has.
    pub last_error: Option<String>,
    /// Seconds since a frame last reached the wire.
    pub last_frame_ago: Option<f64>,
}

#[derive(Default)]
struct LinkSlot {
    out: Option<SenderOutput>,
    /// What the link was built for, so a changed address rebuilds it and an
    /// unchanged one does not.
    key: String,
    /// Sessions the **current** link had the last time we looked.
    sessions: u32,
    /// Sessions banked from every link this panel has already closed (card
    /// 171), so "how many times has this panel's stream come up" is a panel
    /// lifetime number rather than a per-link one.
    closed_ups: u64,
    /// Of those, the ones the **studio** caused rather than the panel: output
    /// switched off and on again, or the studio learning where a typed address
    /// really is. Subtracted from the reconnect count.
    studio_ups: u64,
    /// Whether the current link was built from a resolved device.
    from_resolved: bool,
    /// Ask the device for the brightness policy again on the next pass.
    reapply_brightness: bool,
    /// When a frame last reached the wire.
    last_frame_unix: Option<u64>,
    health: LinkHealth,
}

impl LinkSlot {
    /// Drop the current link and bank what it reached (card 171).
    ///
    /// `studio_took_a_live_stream` says the studio is replacing a link that is
    /// **up right now**, so the connect that follows is this studio getting
    /// back what it just let go of, not the panel coming back.
    fn close_link(&mut self, reached: u32, studio_took_a_live_stream: bool) {
        self.closed_ups += u64::from(reached);
        if studio_took_a_live_stream {
            self.studio_ups += 1;
        }
        self.out = None;
        self.sessions = 0;
    }
}

/// Holds a channel open while a panel fades away from it (card 350): the
/// channel keeps rendering at the full rate and is not dropped until this is.
pub struct Leaving(Arc<Channel>);

impl Leaving {
    fn new(channel: Arc<Channel>) -> Leaving {
        channel.leaving_started();
        Leaving(channel)
    }
}

impl Drop for Leaving {
    fn drop(&mut self) {
        self.0.leaving_done();
    }
}

/// What a panel is fading *from*.
enum FadeFrom {
    /// The channel it has just left, still running on its own clock.
    Channel(Leaving),
    /// A still: black when there was nothing before, or the frame that was
    /// showing when the panel changed channel again mid-fade.
    Still(Frame),
}

struct PanelFade {
    from: FadeFrom,
    clock: Crossfade,
}

/// A panel's output stage: everything between "a channel's linear frame" and
/// "the wire". Owned by the panel, not by any render thread, so a core that is
/// abandoned or a channel that is changed never costs the limiter its history.
struct Stage {
    pipeline: Pipeline,
    fade: Option<PanelFade>,
    /// The last mixed frame, kept only while a fade runs: the still a change
    /// mid-fade fades from.
    last: Option<Frame>,
    limits_at: Instant,
}

impl Stage {
    /// Start fading from what this panel was showing, over `len` seconds.
    fn fade_from(&mut self, old: Option<Arc<Channel>>, len: f64) {
        let clock = Crossfade::new(len);
        if clock.done() {
            self.fade = None;
            self.last = None;
            return;
        }
        let from = match (self.fade.take(), old) {
            // Fades never chain: a change mid-fade holds what is showing.
            (Some(_), _) => FadeFrom::Still(self.last.take().unwrap_or_else(Frame::black)),
            (None, Some(channel)) => FadeFrom::Channel(Leaving::new(channel)),
            (None, None) => FadeFrom::Still(Frame::black()),
        };
        self.fade = Some(PanelFade { from, clock });
    }

    /// The frame for the pipeline: `incoming`, or `incoming` mixed with what
    /// this panel is fading from.
    fn mix(&mut self, incoming: &Frame, wall: f64) -> Frame {
        let Some(fade) = self.fade.as_mut() else {
            return incoming.clone();
        };
        fade.clock.advance(wall);
        if fade.clock.done() {
            // Over: the incoming frame goes through untouched, so an indexed
            // patch is exact again from this frame on - and the channel it
            // left is let go.
            self.fade = None;
            self.last = None;
            return incoming.clone();
        }
        let w = fade.clock.weight();
        let out = match &fade.from {
            FadeFrom::Still(still) => blend(still, incoming, w),
            FadeFrom::Channel(Leaving(old)) => match old.latest() {
                Some(from) => blend(&from, incoming, w),
                // The channel it left has not put a frame out since it was
                // left: hold what was on the panel instead.
                None => blend(self.last.as_ref().unwrap_or(incoming), incoming, w),
            },
        };
        self.last = Some(out.clone());
        out
    }

    #[cfg(test)]
    fn fading(&self) -> bool {
        self.fade.is_some()
    }
}

/// One panel.
pub struct Panel {
    cfg: Mutex<PanelCfg>,
    /// The channel it follows; `None` is idle. Changed only by
    /// [`crate::panels::Panels`], which also keeps the channel's follower list.
    channel: Mutex<Option<Arc<Channel>>>,
    stage: Mutex<Stage>,
    link: Mutex<LinkSlot>,
    /// This panel's preview cell: the frames it is sent, for the browsers.
    screen: Arc<Screen>,
    /// The frame packet's sequence number.
    seq: AtomicU32,
    /// Frames presented to this panel since it was made.
    presents: AtomicU64,
}

impl Panel {
    /// A panel as the state file has it. Its first picture fades in from
    /// black, which is the studio's first picture and a new panel's alike.
    #[must_use]
    pub fn new(stored: &StoredPanel, meter: &Arc<SocketMeter>) -> Arc<Panel> {
        let mut stage = Stage { pipeline: Pipeline::new(stored.output), fade: None, last: None, limits_at: Instant::now() - Duration::from_secs(10) };
        stage.fade_from(None, f64::from(FADE_MANUAL));
        Arc::new(Panel {
            cfg: Mutex::new(PanelCfg { device: stored.device.clone(), on: stored.on, output: stored.output, brightness: stored.brightness }),
            channel: Mutex::new(None),
            stage: Mutex::new(stage),
            link: Mutex::new(LinkSlot::default()),
            screen: Screen::new(Arc::clone(meter)),
            seq: AtomicU32::new(0),
            presents: AtomicU64::new(0),
        })
    }

    fn cfg_mut(&self) -> MutexGuard<'_, PanelCfg> {
        self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn slot(&self) -> MutexGuard<'_, LinkSlot> {
        self.link.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn stage_mut(&self) -> MutexGuard<'_, Stage> {
        self.stage.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The device this is. [`UNBOUND`] for the stand-in.
    #[must_use]
    pub fn device(&self) -> String {
        self.cfg_mut().device.clone()
    }

    /// True for the stand-in panel of a studio that has found no panel yet.
    #[must_use]
    pub fn is_unbound(&self) -> bool {
        self.cfg_mut().device == UNBOUND
    }

    #[must_use]
    pub fn cfg(&self) -> PanelCfg {
        self.cfg_mut().clone()
    }

    /// The channel it follows, if it is not idle.
    #[must_use]
    pub fn channel(&self) -> Option<Arc<Channel>> {
        self.channel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Follow `next` instead of what it follows now, fading from the old
    /// picture over `fade` seconds. Only [`crate::panels::Panels`] calls this,
    /// under its own lock, and keeps the channels' follower lists with it.
    pub(crate) fn switch_channel(self: &Arc<Self>, next: Option<Arc<Channel>>, fade: f64) -> Option<Arc<Channel>> {
        let old = {
            let mut slot = self.channel.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::replace(&mut *slot, next.clone())
        };
        if let Some(o) = &old {
            o.remove_follower(self);
        }
        if let Some(n) = &next {
            n.add_follower(self);
        }
        self.stage_mut().fade_from(old.clone(), fade);
        old
    }

    /// Let go of its channel at once, with no fade: the panel is going away.
    pub(crate) fn drop_channel(self: &Arc<Self>) {
        let old = self.channel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(o) = old {
            o.remove_follower(self);
        }
        let mut stage = self.stage_mut();
        stage.fade = None;
        stage.last = None;
    }

    /// The panel as the state file keeps it.
    #[must_use]
    pub fn stored(&self) -> StoredPanel {
        let cfg = self.cfg();
        StoredPanel { device: cfg.device, on: cfg.on, channel: self.channel().map(|c| c.id()), output: cfg.output, brightness: cfg.brightness }
    }

    /// Rename this panel onto a device: the stand-in adopted by the first
    /// panel somebody named, or a `pending:` id becoming the device's real
    /// one. **Nothing else is touched**, so the picture carries straight on.
    pub fn rename(&self, device: &str) {
        self.cfg_mut().device = device.to_string();
    }

    pub fn set_on(&self, on: bool) {
        self.cfg_mut().on = on;
    }

    /// The output stage's settings. Picked up by the pipeline on the next
    /// frame.
    pub fn set_output(&self, output: Output) {
        self.cfg_mut().output = output;
    }

    /// `None` clears the brightness policy; `Some(n)` sets it, to be applied
    /// on the supervisor's next pass.
    pub fn set_brightness(&self, level: Option<u8>) {
        self.cfg_mut().brightness = level;
        // Re-apply on the next pass (card 171: never by inventing a session).
        self.slot().reapply_brightness = true;
    }

    /// This panel's preview cell.
    #[must_use]
    pub fn screen(&self) -> Arc<Screen> {
        Arc::clone(&self.screen)
    }

    /// Frames its channel has handed it.
    #[must_use]
    pub fn presents(&self) -> u64 {
        self.presents.load(Ordering::Relaxed)
    }

    /// **One frame of its channel's**, `wall` seconds after the last one:
    /// mixed while fading, through this panel's pipeline, into its preview
    /// cell and onto its link. Called by the channel's render thread; never
    /// blocks on a browser or the network.
    ///
    /// True when this panel wants the full frame rate: its link is up, or
    /// somebody is watching its preview.
    pub fn present(&self, frame: &Frame, wall: f64, t: f64, fps: f32) -> bool {
        let output = self.cfg_mut().output;
        let mut stage = self.stage_mut();
        if stage.pipeline.output != output {
            stage.pipeline.output = output;
        }
        let mixed = stage.mix(frame, wall);
        let out = stage.pipeline.process(mixed, wall);
        let seq = self.seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        // One slot, replaced in place: a browser that is not keeping up misses
        // frames and costs nothing.
        self.screen.show(page::pack(seq, t, &out.stats, fps, &out.preview));
        self.presents.fetch_add(1, Ordering::Relaxed);

        let connected = {
            let mut slot = self.slot();
            let mut landed = false;
            let mut failed = None;
            let up = match slot.out.as_mut() {
                Some(link) => {
                    if stage.limits_at.elapsed() >= Duration::from_secs(1) {
                        stage.limits_at = Instant::now();
                        let lim = link.limits();
                        if lim.connected {
                            stage.pipeline.meter().set_limits(lim.budget, lim.codecs.clone());
                        }
                    }
                    let before = link.link().stats().frames_sent;
                    if let Err(e) = link.send(&out.wire) {
                        // Only our own errors can get here; the network cannot
                        // fail a send.
                        failed = Some(format!("sending to the panel: {e}"));
                    }
                    landed = link.link().stats().frames_sent > before;
                    link.link().state().is_up()
                }
                None => false,
            };
            if landed {
                slot.last_frame_unix = Some(unix_now());
            }
            if let Some(msg) = failed {
                // Logged once, not once a frame.
                if slot.health.last_error.as_deref() != Some(msg.as_str()) {
                    eprintln!("studio: panel {}: {msg}", label(&self.device()));
                }
                slot.health.last_error = Some(msg);
            }
            up
        };
        connected || self.screen.watchers() > 0
    }

    /// Point the link at a device, or at nothing. A link exists only while
    /// output is on **and** the panel has a picture (an idle panel streams
    /// nothing, so the device shows its own screen). Rebuilds it only when
    /// what it is aimed at has actually changed.
    ///
    /// Card 171: this is also where the reconnect count is kept honest.
    pub fn aim(&self, reach: &Reach) {
        let on = self.cfg_mut().on && self.channel().is_some();
        let key = reach_key(reach);
        let mut slot = self.slot();
        if !on || matches!(reach, Reach::Unknown) {
            if let Some(out) = slot.out.as_mut() {
                // Switching output off takes away a stream that was running,
                // so the connect that follows switching it back on is not the
                // panel coming back. Losing the device's address is the
                // opposite: the panel really is away.
                let took_a_live_stream = !on && out.link().state().is_up();
                let reached = out.link().stats().sessions;
                // FINAL: the panel is released now rather than after its
                // stream timeout, and goes back to its own idle screen.
                out.close();
                slot.close_link(reached, took_a_live_stream);
            }
            slot.key = String::new();
            slot.from_resolved = false;
            return;
        }
        if slot.out.is_some() && slot.key == key {
            return;
        }
        let resolved = matches!(reach, Reach::Resolved(_));
        if let Some(out) = slot.out.as_ref() {
            // The studio finding out where the device really is: a panel typed
            // in as an address is re-aimed at the resolved device within
            // seconds of being added, and has not moved an inch - but only
            // while the stream it replaces is **up**.
            let took_a_live_stream = resolved && !slot.from_resolved && out.link().state().is_up();
            let reached = out.link().stats().sessions;
            slot.close_link(reached, took_a_live_stream);
        }
        slot.out = Some(open_link(reach));
        slot.key = key;
        slot.sessions = 0;
        slot.from_resolved = resolved;
    }

    /// Drive reconnection and read the link even when frames are not flowing,
    /// about once a second, and notice a new session.
    ///
    /// Returns a brightness to apply if the policy says so and the link has
    /// just come up - done by the caller, off this thread, because a control
    /// request can take a second.
    pub fn supervise(&self) -> Option<BrightnessJob> {
        let (want, device) = {
            let cfg = self.cfg_mut();
            (cfg.brightness, cfg.device.clone())
        };
        let mut job = None;
        let mut slot = self.slot();
        let last = slot.sessions;
        let reapply = slot.reapply_brightness;
        let mut sessions = 0;
        let mut up = false;
        if let Some(out) = slot.out.as_mut() {
            out.poll();
            sessions = out.link().stats().sessions;
            up = out.link().state().is_up();
        }
        if up && (sessions != last || reapply) {
            slot.reapply_brightness = false;
            if let Some(level) = want {
                job = Some(BrightnessJob { device, level });
            }
        }
        slot.sessions = sessions;
        // Card 171: the count a person reads is a *panel* lifetime one.
        let link_ups = slot.closed_ups + u64::from(sessions);
        let reconnects = link_ups.saturating_sub(1 + slot.studio_ups);
        slot.health.sessions = u64::from(sessions);
        slot.health.link_ups = link_ups;
        slot.health.reconnects = reconnects;
        job
    }

    /// Record what the device actually applied, which may differ from what
    /// was asked for in either direction: below because the firmware cap is
    /// lower, or above because a nonzero request landed under the floor
    /// (cards 136, 187). Only the cap is a ceiling worth remembering.
    pub fn brightness_applied(&self, asked: u8, applied: u8) {
        {
            let mut slot = self.slot();
            slot.health.brightness_applied = Some(applied);
            if applied < asked {
                slot.health.brightness_cap = Some(applied);
            }
        }
        // The policy becomes what the panel actually does.
        let mut cfg = self.cfg_mut();
        if cfg.brightness == Some(asked) && applied != asked {
            cfg.brightness = Some(applied);
        }
    }

    /// The brightness policy, and what the device last said it applied.
    #[must_use]
    pub fn brightness_policy(&self) -> (Option<u8>, Option<u8>) {
        let want = self.cfg_mut().brightness;
        (want, self.slot().health.brightness_applied)
    }

    /// Card 164: what this panel's link has put on the wire and taken off it,
    /// over the life of the link object.
    #[must_use]
    pub fn link_traffic(&self) -> screeny::LinkTraffic {
        self.slot().out.as_ref().map_or_else(Default::default, |o| o.link().stats().traffic)
    }

    /// The link, or `None` when there is none (output off, or idle).
    #[must_use]
    pub fn link_status(&self) -> Option<PanelStatus> {
        self.slot().out.as_ref().map(SenderOutput::status)
    }

    /// The link's side of `health`.
    #[must_use]
    pub fn link_health(&self) -> LinkHealth {
        let slot = self.slot();
        let mut h = slot.health.clone();
        h.last_frame_ago = slot.last_frame_unix.map(|t| unix_now().saturating_sub(t) as f64);
        h
    }

    /// Release the device: `FINAL`, and no more link.
    pub fn shutdown(&self) {
        let mut slot = self.slot();
        if let Some(out) = slot.out.as_mut() {
            out.close();
        }
        slot.out = None;
    }

    /// Test only: fade from the channel it follows now to `next`, without the
    /// registry, so a test can drive [`Panel::present`] by hand.
    #[cfg(test)]
    pub(crate) fn fading(&self) -> bool {
        self.stage_mut().fading()
    }
}

/// A device id for a log line.
fn label(device: &str) -> &str {
    if device.is_empty() {
        "(no panel yet)"
    } else {
        device
    }
}

/// Build the link for a way of reaching a device.
fn open_link(reach: &Reach) -> SenderOutput {
    match reach {
        // Resolved by the registry: attach to exactly these two ports and
        // never browse (card 111). It survives a reboot at the same address.
        Reach::Resolved(d) => SenderOutput::attach_deferred((**d).clone(), screeny::LinkConfig::default()),
        // A name a human typed: re-resolved on every reconnect, so it follows
        // the device across a DHCP lease.
        Reach::Name(n) => SenderOutput::deferred(screeny_art::output::target_for(n)),
        Reach::Addr(a) => SenderOutput::deferred(screeny_art::output::target_for(&a.to_string())),
        Reach::Unknown => SenderOutput::deferred(screeny_art::output::target_for("")),
    }
}

fn reach_key(reach: &Reach) -> String {
    match reach {
        Reach::Resolved(d) => format!("dev:{}|{}", d.frame, d.control),
        Reach::Name(n) => format!("name:{n}"),
        Reach::Addr(a) => format!("addr:{a}"),
        Reach::Unknown => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StoredChannel;
    use screeny_art::{Rgb, N};

    const RED: Rgb = Rgb::new(0.30, 0.02, 0.01);
    const BLUE: Rgb = Rgb::new(0.01, 0.03, 0.25);
    const DT: f64 = 1.0 / 30.0;

    fn flat(c: Rgb) -> Frame {
        Frame::Indexed { palette: vec![c], indices: vec![0; N] }
    }

    fn close(a: Rgb, b: Rgb) -> bool {
        (a.r - b.r).abs() < 1e-5 && (a.g - b.g).abs() < 1e-5 && (a.b - b.b).abs() < 1e-5
    }

    fn panel(device: &str) -> Arc<Panel> {
        Panel::new(&StoredPanel { device: device.into(), on: false, ..StoredPanel::default() }, &Arc::new(SocketMeter::default()))
    }

    /// Card 350: **a panel that changes channel fades** from the old
    /// channel's picture to the new one's over the manual length, in its own
    /// output stage - and lands exactly on the new one when it is over, with
    /// the old channel let go.
    #[test]
    fn a_panel_changing_channel_fades_from_the_old_picture_to_the_new() {
        let red = Channel::new(StoredChannel { id: 1, ..StoredChannel::default() }, false);
        let blue = Channel::new(StoredChannel { id: 2, ..StoredChannel::default() }, false);
        red.set_latest(flat(RED));
        let p = panel("abc");
        // On red, with its first fade in from black already over.
        p.switch_channel(Some(Arc::clone(&red)), 0.0);
        let mut stage = p.stage_mut();
        assert!(!stage.fading());
        drop(stage);

        p.switch_channel(Some(Arc::clone(&blue)), f64::from(FADE_MANUAL));
        assert!(!red.unused(), "a panel fading away keeps the old channel alive");
        assert!(red.follower_ids().is_empty() && blue.follower_ids() == vec!["abc".to_string()]);
        let mut frames = 0;
        loop {
            stage = p.stage_mut();
            let f = stage.mix(&flat(BLUE), DT);
            let fading = stage.fading();
            drop(stage);
            frames += 1;
            if !fading {
                assert!(matches!(f, Frame::Indexed { .. }), "after the fade the new channel's frame goes through untouched");
                break;
            }
            let w = screeny_art::crossfade::ease((frames as f64 * DT / f64::from(FADE_MANUAL)) as f32);
            assert!(close(f.pixel(7), RED.lerp(BLUE, w)), "frame {frames}: {:?} is not {w} of the way", f.pixel(7));
            assert!(frames < 100, "the fade never ended");
        }
        assert!((59..=61).contains(&frames), "two seconds at 30 fps, not {frames} frames");
        assert!(red.unused(), "and the old channel is let go once the fade is over");
    }

    /// A change of channel mid-fade holds what was showing as a still rather
    /// than chaining fades, and lets go of the first channel at once.
    #[test]
    fn a_second_change_mid_fade_fades_from_a_still() {
        let red = Channel::new(StoredChannel { id: 1, ..StoredChannel::default() }, false);
        let blue = Channel::new(StoredChannel { id: 2, ..StoredChannel::default() }, false);
        let green = Channel::new(StoredChannel { id: 3, ..StoredChannel::default() }, false);
        red.set_latest(flat(RED));
        let p = panel("abc");
        p.switch_channel(Some(Arc::clone(&red)), 0.0);
        p.switch_channel(Some(Arc::clone(&blue)), 2.0);
        let mut shown = Frame::black();
        for _ in 0..30 {
            shown = p.stage_mut().mix(&flat(BLUE), DT);
        }
        p.switch_channel(Some(Arc::clone(&green)), 2.0);
        assert!(red.unused(), "the first channel is not held by a fade from a still");
        let f = p.stage_mut().mix(&flat(Rgb::new(0.02, 0.2, 0.03)), DT);
        let w = screeny_art::crossfade::ease((DT / 2.0) as f32);
        assert!(close(f.pixel(0), shown.pixel(0).lerp(Rgb::new(0.02, 0.2, 0.03), w)), "from the still that was showing");
    }

    /// An idle panel's link is never built, whatever `on` says: it has nothing
    /// to send, so the device shows its own screen.
    #[test]
    fn an_idle_panel_has_no_link() {
        let p = panel("abc");
        p.set_on(true);
        p.aim(&Reach::Addr("127.0.0.1:50999".parse().expect("an address")));
        assert!(p.link_status().is_none(), "idle: no channel, no link");
        let c = Channel::new(StoredChannel { id: 1, ..StoredChannel::default() }, false);
        p.switch_channel(Some(c), 0.0);
        p.aim(&Reach::Addr("127.0.0.1:50999".parse().expect("an address")));
        assert!(p.link_status().is_some(), "a picture and output on: a link");
        p.set_on(false);
        p.aim(&Reach::Addr("127.0.0.1:50999".parse().expect("an address")));
        assert!(p.link_status().is_none(), "output off releases it");
        p.shutdown();
    }

    /// Cards 136 and 187, unchanged by the split: a raise by the floor moves
    /// the policy without being learned as a cap; a cap pulls it down and is.
    #[test]
    fn brightness_applied_moves_the_policy_honestly() {
        let p = panel("abc");
        p.set_brightness(Some(3));
        p.brightness_applied(3, 6);
        assert_eq!(p.brightness_policy(), (Some(6), Some(6)));
        assert_eq!(p.link_health().brightness_cap, None, "a raise is not a cap");
        p.set_brightness(Some(200));
        p.brightness_applied(200, 120);
        assert_eq!(p.brightness_policy(), (Some(120), Some(120)));
        assert_eq!(p.link_health().brightness_cap, Some(120));
    }
}
