//! Panels: a device, and what is about the device alone (cards 350, 353).
//!
//! A panel is a **member of a channel**: its device, its name (the registry's),
//! the [`Channel`] it is on - always one - on/off, its brightness and its link.
//! It does not render, limit, quantise or encode: its channel does all of that
//! once per tick and hands every member the same [`ChannelFrame`], whose
//! encoded payload the panel's link sends **byte for byte**
//! (`docs/design/studio-vision.md`, "Several panels: channels own the
//! picture"). Brightness is applied on the device, so it never changes the
//! frames: two mirrored panels can run at different levels.
//!
//! **Moving between channels fades.** A panel that changes channel keeps the
//! old channel's frames for [`crate::channel::FADE_MANUAL`] and blends them into the new one's
//! with `crossfade::blend`, in linear light, through an output stage of its
//! own that carries on from the old channel's limiter ([`Pipeline::fork`]) and
//! is finished with the new channel's output settings. For those two seconds,
//! and only for that panel, its frames are its own; then it is back on the
//! shared bytes. A change mid-fade fades from a still of what was showing,
//! never chaining a second fade onto the first. The old channel is told a
//! panel is leaving it, so it keeps rendering at the full rate until the fade
//! is over. A new panel fades in from black the same way.

use screeny_art::crossfade::{blend, Crossfade};
use screeny_art::output::{Output as FrameSink, PanelStatus, SenderOutput};
use screeny_art::{Frame, Output, Pipeline};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::channel::{Channel, ChannelFrame};
use crate::devices::Reach;
use crate::state::{unix_now, StoredPanel};

/// A brightness the caller should apply, off the render thread.
pub struct BrightnessJob {
    pub device: String,
    pub level: u8,
}

/// What a panel is set to, as persisted - less the channel it is on, which is
/// [`Panel::channel`].
#[derive(Clone, Debug, PartialEq)]
pub struct PanelCfg {
    /// The device id.
    pub device: String,
    /// **Panel output.** False releases the link - the panel goes back to its
    /// own idle screen - and the channel carries on for the page and for the
    /// other panels on it.
    pub on: bool,
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

/// Card 353: how this panel's frames were made - the proof, in counters, that
/// panels on one channel are sent the same bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct SendCounts {
    /// Frames its channel handed it.
    pub presented: u64,
    /// Of those, frames sent as the channel's **shared encoded payload**, byte
    /// for byte.
    pub shared: u64,
    /// Frames it encoded for itself: its two seconds of fade when it changes
    /// channel (or arrives), and any frame its live session could not take as
    /// the channel encoded it (a smaller budget, for the moment before the
    /// channel's encoder is pointed at it).
    pub own: u64,
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

/// Holds a channel at the full rate while a panel fades away from it (card
/// 350): it keeps rendering, and is not shut down, until this is dropped.
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

/// A panel's two seconds of frames of its own (card 353).
struct PanelFade {
    from: FadeFrom,
    clock: Crossfade,
    /// Its own output stage for the fade: carried on from the channel it left,
    /// finished with the output settings of the channel it is going to.
    pipeline: Pipeline,
    /// The last mixed frame: the still a change mid-fade fades from.
    last: Option<Frame>,
}

impl PanelFade {
    /// The frame to finish: `incoming` mixed with what this panel is fading
    /// from. `None` once the fade is over.
    fn mix(&mut self, incoming: &Frame, wall: f64) -> Option<Frame> {
        self.clock.advance(wall);
        if self.clock.done() {
            return None;
        }
        let w = self.clock.weight();
        let out = match &self.from {
            FadeFrom::Still(still) => blend(still, incoming, w),
            FadeFrom::Channel(Leaving(old)) => match old.latest() {
                Some(from) => blend(&from, incoming, w),
                // The channel it left has not put a frame out since it was
                // left: hold what was on the panel instead.
                None => blend(self.last.as_ref().unwrap_or(incoming), incoming, w),
            },
        };
        self.last = Some(out.clone());
        Some(out)
    }
}

/// One panel.
pub struct Panel {
    cfg: Mutex<PanelCfg>,
    /// The channel it is on. Always one while the panel is in the studio;
    /// `None` only once it has been forgotten. Changed only by
    /// [`crate::panels::Panels`], which also keeps the channels' member lists.
    channel: Mutex<Option<Arc<Channel>>>,
    /// Its own frames, while it fades onto a channel.
    fade: Mutex<Option<PanelFade>>,
    link: Mutex<LinkSlot>,
    presented: AtomicU64,
    shared: AtomicU64,
    own: AtomicU64,
}

impl Panel {
    /// A panel as the state file has it, on no channel yet: the caller puts
    /// it on one with [`Panel::switch_channel`], which fades it in from
    /// black.
    #[must_use]
    pub fn new(stored: &StoredPanel) -> Arc<Panel> {
        Arc::new(Panel {
            cfg: Mutex::new(PanelCfg { device: stored.device.clone(), on: stored.on, brightness: stored.brightness }),
            channel: Mutex::new(None),
            fade: Mutex::new(None),
            link: Mutex::new(LinkSlot::default()),
            presented: AtomicU64::new(0),
            shared: AtomicU64::new(0),
            own: AtomicU64::new(0),
        })
    }

    fn cfg_mut(&self) -> MutexGuard<'_, PanelCfg> {
        self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn slot(&self) -> MutexGuard<'_, LinkSlot> {
        self.link.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn fade_mut(&self) -> MutexGuard<'_, Option<PanelFade>> {
        self.fade.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The device this is.
    #[must_use]
    pub fn device(&self) -> String {
        self.cfg_mut().device.clone()
    }

    #[must_use]
    pub fn cfg(&self) -> PanelCfg {
        self.cfg_mut().clone()
    }

    /// The channel it is on.
    #[must_use]
    pub fn channel(&self) -> Option<Arc<Channel>> {
        self.channel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// True while it is fading onto its channel: its frames are its own.
    #[must_use]
    pub fn fading(&self) -> bool {
        self.fade_mut().is_some()
    }

    /// Go onto `next`, fading from what it was showing over `fade` seconds.
    /// Only [`crate::panels::Panels`] calls this, under its own lock, and
    /// keeps the channels' member lists with it.
    pub(crate) fn switch_channel(self: &Arc<Self>, next: &Arc<Channel>, fade: f64) -> Option<Arc<Channel>> {
        let old = {
            let mut slot = self.channel.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            slot.replace(Arc::clone(next))
        };
        if let Some(o) = &old {
            o.remove_follower(self);
        }
        next.add_follower(self);
        let clock = Crossfade::new(fade);
        let mut slot = self.fade_mut();
        if clock.done() {
            *slot = None;
            return old;
        }
        let (from, pipeline) = match (slot.take(), &old) {
            // Fades never chain: a change mid-fade holds what is showing, and
            // keeps the output stage that was showing it.
            (Some(prev), _) => (FadeFrom::Still(prev.last.unwrap_or_else(Frame::black)), prev.pipeline),
            (None, Some(channel)) => (FadeFrom::Channel(Leaving::new(Arc::clone(channel))), channel.fork_pipeline()),
            (None, None) => (FadeFrom::Still(Frame::black()), Pipeline::new(next.output())),
        };
        *slot = Some(PanelFade { from, clock, pipeline, last: None });
        old
    }

    /// Let go of its channel at once, with no fade: the panel is going away.
    pub(crate) fn drop_channel(self: &Arc<Self>) {
        let old = self.channel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(o) = old {
            o.remove_follower(self);
        }
        *self.fade_mut() = None;
    }

    /// The panel as the state file keeps it.
    #[must_use]
    pub fn stored(&self) -> StoredPanel {
        let cfg = self.cfg();
        StoredPanel {
            device: cfg.device,
            on: cfg.on,
            channel: self.channel().map_or(crate::state::HOME_CHANNEL, |c| c.id()),
            output: None,
            brightness: cfg.brightness,
        }
    }

    /// Rename this panel onto a device: a `pending:` id becoming the device's
    /// real one. **Nothing else is touched**, so the picture carries straight
    /// on.
    pub fn rename(&self, device: &str) {
        self.cfg_mut().device = device.to_string();
    }

    pub fn set_on(&self, on: bool) {
        self.cfg_mut().on = on;
    }

    /// `None` clears the brightness policy; `Some(n)` sets it, to be applied
    /// on the supervisor's next pass.
    pub fn set_brightness(&self, level: Option<u8>) {
        self.cfg_mut().brightness = level;
        // Re-apply on the next pass (card 171: never by inventing a session).
        self.slot().reapply_brightness = true;
    }

    /// Card 353: how its frames were made - shared, or its own.
    #[must_use]
    pub fn sends(&self) -> SendCounts {
        SendCounts {
            presented: self.presented.load(Ordering::Relaxed),
            shared: self.shared.load(Ordering::Relaxed),
            own: self.own.load(Ordering::Relaxed),
        }
    }

    /// Frames its channel has handed it.
    #[must_use]
    pub fn presents(&self) -> u64 {
        self.presented.load(Ordering::Relaxed)
    }

    /// The limits of its link, while the link is up: what its channel's
    /// encoder has to fit (card 353).
    #[must_use]
    pub fn link_limits(&self) -> Option<screeny::Limits> {
        let slot = self.slot();
        let lim = slot.out.as_ref()?.limits();
        lim.connected.then_some(lim)
    }

    /// **One tick of its channel's**, `wall` seconds after the last one:
    /// the channel's shared encoded frame onto this panel's link, byte for
    /// byte - or, while this panel is fading onto the channel, a frame of its
    /// own (mixed, finished with `output`, encoded by its link). Called by the
    /// channel's render thread; never blocks on a browser or the network.
    ///
    /// True when this panel's link is up, which is a reason for its channel
    /// to render at the full rate.
    pub fn present(&self, frame: &ChannelFrame, wall: f64, output: Output) -> bool {
        self.presented.fetch_add(1, Ordering::Relaxed);
        // The fade, if there is one: a frame of this panel's own.
        let own = {
            let mut slot = self.fade_mut();
            let mixed = slot.as_mut().and_then(|f| f.mix(&frame.linear, wall).map(|m| (m, f)));
            match mixed {
                Some((mixed, fade)) => {
                    if fade.pipeline.output != output {
                        fade.pipeline.output = output;
                    }
                    Some(fade.pipeline.process(mixed, wall).wire)
                }
                None => {
                    // Over (or there was none): back on the shared bytes.
                    *slot = None;
                    None
                }
            }
        };

        let mut slot = self.slot();
        let mut landed = false;
        let mut failed = None;
        let up = match slot.out.as_mut() {
            Some(link) => {
                let before = link.link().stats().frames_sent;
                let sent = match &own {
                    Some(wire) => link.send(wire).map(|()| false),
                    None => link.send_shared(&frame.wire, &frame.encoded, frame.exact, frame.indexed),
                };
                match sent {
                    Ok(true) => {
                        self.shared.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(false) => {
                        self.own.fetch_add(1, Ordering::Relaxed);
                    }
                    // Only our own errors can get here; the network cannot
                    // fail a send.
                    Err(e) => failed = Some(format!("sending to the panel: {e}")),
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
                eprintln!("studio: panel {}: {msg}", self.device());
            }
            slot.health.last_error = Some(msg);
        }
        up
    }

    /// Point the link at a device, or at nothing. A link exists only while
    /// output is on **and** the panel is on a channel (which it always is,
    /// until it is forgotten). Rebuilds it only when what it is aimed at has
    /// actually changed.
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
    use crate::channel::FADE_MANUAL;
    use crate::page::SocketMeter;
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
        Panel::new(&StoredPanel { device: device.into(), on: false, ..StoredPanel::default() })
    }

    fn channel(id: u32) -> Arc<Channel> {
        Channel::new(StoredChannel { id, ..StoredChannel::default() }, false, &Arc::new(SocketMeter::default()))
    }

    /// Mix one frame of `incoming` through the panel's fade, as `present`
    /// does: `None` once the fade is over and the panel is back on the shared
    /// bytes.
    fn mix(p: &Panel, incoming: &Frame) -> Option<Frame> {
        let mut slot = p.fade_mut();
        let out = slot.as_mut().and_then(|f| f.mix(incoming, DT));
        if out.is_none() {
            *slot = None;
        }
        out
    }

    /// Card 350, kept by 353: **a panel that changes channel fades** from the
    /// old channel's picture to the new one's over the manual length - frames
    /// of its own - and then is back on the channel's shared frames, with the
    /// old channel let go.
    #[test]
    fn a_panel_changing_channel_fades_from_the_old_picture_to_the_new() {
        let red = channel(1);
        let blue = channel(2);
        red.set_latest(flat(RED));
        let p = panel("abc");
        // On red, with no fade.
        p.switch_channel(&red, 0.0);
        assert!(!p.fading());

        p.switch_channel(&blue, f64::from(FADE_MANUAL));
        assert!(p.fading(), "its frames are its own while it fades");
        assert!(!red.unused(), "a panel fading away keeps the old channel going");
        assert!(red.follower_ids().is_empty() && blue.follower_ids() == vec!["abc".to_string()]);
        let mut frames = 0;
        while let Some(f) = mix(&p, &flat(BLUE)) {
            frames += 1;
            let w = screeny_art::crossfade::ease((frames as f64 * DT / f64::from(FADE_MANUAL)) as f32);
            assert!(close(f.pixel(7), RED.lerp(BLUE, w)), "frame {frames}: {:?} is not {w} of the way", f.pixel(7));
            assert!(frames < 100, "the fade never ended");
        }
        assert!((58..=61).contains(&frames), "two seconds at 30 fps, not {frames} frames");
        assert!(!p.fading(), "and then it is back on the shared bytes");
        assert!(red.unused(), "and the old channel is let go once the fade is over");
    }

    /// A change of channel mid-fade holds what was showing as a still rather
    /// than chaining fades, and lets go of the first channel at once.
    #[test]
    fn a_second_change_mid_fade_fades_from_a_still() {
        let red = channel(1);
        let blue = channel(2);
        let green = channel(3);
        red.set_latest(flat(RED));
        let p = panel("abc");
        p.switch_channel(&red, 0.0);
        p.switch_channel(&blue, 2.0);
        let mut shown = Frame::black();
        for _ in 0..30 {
            shown = mix(&p, &flat(BLUE)).expect("still fading");
        }
        p.switch_channel(&green, 2.0);
        assert!(red.unused(), "the first channel is not held by a fade from a still");
        let f = mix(&p, &flat(Rgb::new(0.02, 0.2, 0.03))).expect("fading again");
        let w = screeny_art::crossfade::ease((DT / 2.0) as f32);
        assert!(close(f.pixel(0), shown.pixel(0).lerp(Rgb::new(0.02, 0.2, 0.03), w)), "from the still that was showing");
    }

    /// A new panel fades in from black - frames of its own - onto its channel.
    #[test]
    fn a_new_panel_fades_in_from_black() {
        let blue = channel(1);
        let p = panel("abc");
        p.switch_channel(&blue, f64::from(FADE_MANUAL));
        let first = mix(&p, &flat(BLUE)).expect("fading in");
        assert!(first.pixel(0).b < BLUE.b * 0.01, "the first frame is all but black: {:?}", first.pixel(0));
    }

    /// Output off releases the link; on, with a channel, builds one.
    #[test]
    fn output_off_releases_the_link() {
        let p = panel("abc");
        p.set_on(true);
        p.switch_channel(&channel(1), 0.0);
        p.aim(&Reach::Addr("127.0.0.1:50999".parse().expect("an address")));
        assert!(p.link_status().is_some(), "on a channel and output on: a link");
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
