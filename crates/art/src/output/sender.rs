//! Frames to a real panel, over the wire, exactly when the piece made them
//! exactly.
//!
//! This is the whole of the network side of the art system, and it is thin on
//! purpose: `screeny::Link` (card 011) was shaped for this trait. It never
//! sleeps, so the art system keeps its own 60 fps clock; it drops frames that
//! arrive before the panel's next slot and says so (`Sent::Coalesced`); and it
//! cannot fail because of the network, so a panel that reboots, moves address
//! or is simply off is a counter here rather than an `io::Error` a render loop
//! has to decide what to do about.
//!
//! Behind the `sender` feature: without it `screeny-art` has no network stack
//! at all, which is what the headless runner and the tests want.

use crate::frame::WireFrame;
use crate::output::Output;
use screeny::{Device, Limits, Link, LinkConfig, LinkState, Pixels, Sent, Target};
use serde::Serialize;
use std::io;

/// Turn `--to <name-or-addr>` into a [`Target`].
///
/// `10.0.0.5`, `10.0.0.5:49374` and `[::1]:49374` skip discovery entirely;
/// anything else is a DNS-SD instance name (or a unique prefix of one), which
/// is the better answer for a device on DHCP because the link re-resolves it
/// on every reconnect and so follows the device across a lease.
#[must_use]
pub fn target_for(to: &str) -> Target {
    let to = to.trim();
    if let Ok(addr) = to.parse() {
        return Target { addr: Some(addr), ..Target::default() };
    }
    if let Ok(ip) = to.parse() {
        return Target {
            addr: Some(std::net::SocketAddr::new(ip, screeny_proto::DEFAULT_FRAME_PORT)),
            ..Target::default()
        };
    }
    Target { name: Some(to.to_string()), ..Target::default() }
}

/// What became of the last frame, and of every frame so far.
///
/// Serializable because the studio's status strip is the only consumer that
/// matters and it lives on the other side of an IPC boundary. Keeping the
/// shape here rather than in the studio is deliberate: card 105 replaces the
/// Tauri app with a server and inherits this unchanged.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PanelStatus {
    /// What the link was asked to find: a name or an address.
    pub target: String,
    /// `up`, `connecting`, `waiting` or `closed`.
    pub state: &'static str,
    /// True only in `up`.
    pub connected: bool,
    /// The device the link settled on, once it has one.
    pub device: Option<String>,
    /// The rate the panel is keeping up with, which moves (spec 6.9).
    pub fps: f64,
    /// Payload bytes a frame may use against this device.
    pub budget: u32,
    /// Palette size guaranteed exact against this device at this budget.
    pub exact_palette: u32,
    /// Frames handed to the link.
    pub frames_offered: u64,
    /// Frames that reached the wire.
    pub frames_sent: u64,
    /// Frames folded away by the cadence ceiling. Not a problem: a 60 fps
    /// piece into a 30 fps panel coalesces half of them by design.
    pub frames_coalesced: u64,
    /// Frames lost because the link was down.
    pub frames_dropped: u64,
    /// Indexed frames that went out exactly.
    pub indexed_exact: u64,
    /// Indexed frames that had to be requantised because nothing exact fit.
    /// The number the studio's stats strip is for.
    pub indexed_fallback: u64,
    /// Codec of the last frame that reached the wire.
    pub codec: Option<u8>,
    /// The same, in words.
    pub codec_name: Option<&'static str>,
    /// Payload bytes of the last frame that reached the wire.
    pub bytes: Option<u32>,
    /// Whether that frame was exact.
    pub exact: Option<bool>,
    /// The last thing that went wrong, if anything has.
    pub last_error: Option<String>,
}

/// An [`Output`] that puts frames on a panel.
pub struct SenderOutput {
    link: Link,
    label: String,
    last: Option<Sent>,
}

impl SenderOutput {
    /// Connect now, and fail if the panel cannot be found.
    ///
    /// What a command-line run wants: being told "no device answered" beats
    /// streaming into the void for a minute.
    ///
    /// # Errors
    ///
    /// If the panel cannot be found or does not answer the handshake.
    pub fn open(target: Target) -> io::Result<Self> {
        Self::open_with(target, LinkConfig::default())
    }

    /// As [`SenderOutput::open`], with the link configured by the caller.
    ///
    /// # Errors
    ///
    /// If the panel cannot be found or does not answer the handshake.
    pub fn open_with(target: Target, cfg: LinkConfig) -> io::Result<Self> {
        let label = label_of(&target);
        Ok(SenderOutput { link: Link::open(target, cfg)?, label, last: None })
    }

    /// Connect to a panel that is already resolved, ports and all.
    ///
    /// For a caller holding a [`Device`] rather than a way of finding one: the
    /// studio's device list (card 106), or a test pointing at a simulator on
    /// ephemeral ports. The link is **pinned** to that device - it reconnects
    /// to the same two ports and never browses, so it does not follow a DHCP
    /// lease; see `Link::attach`.
    ///
    /// # Errors
    ///
    /// If the panel does not answer the handshake.
    pub fn attach(device: Device, cfg: LinkConfig) -> io::Result<Self> {
        let label = device.label();
        Ok(SenderOutput { link: Link::attach(device, cfg)?, label, last: None })
    }

    /// Start without a panel and pick one up whenever it appears.
    ///
    /// What a service wants: a panel that is off at boot is not a different
    /// situation from one unplugged an hour later. Never fails.
    #[must_use]
    pub fn deferred(target: Target) -> Self {
        let label = label_of(&target);
        SenderOutput {
            link: Link::open_deferred(target, LinkConfig::default()),
            label,
            last: None,
        }
    }

    /// As [`SenderOutput::deferred`], with the link configured by the caller -
    /// a different cadence, a different frame rate, reconnection off.
    #[must_use]
    pub fn deferred_with(target: Target, cfg: LinkConfig) -> Self {
        let label = label_of(&target);
        SenderOutput { link: Link::open_deferred(target, cfg), label, last: None }
    }

    /// Drain telemetry and drive reconnection while frames are not flowing -
    /// between pieces, while one is loading, or when the studio is paused.
    /// [`Output::send`] does this itself.
    pub fn poll(&mut self) {
        self.link.poll();
    }

    /// The rate the panel is keeping up with. Follows spec 6.9's adaptation,
    /// so read it rather than assuming the configured value.
    #[must_use]
    pub fn fps(&self) -> f64 {
        self.link.fps()
    }

    /// Budget, codec set, guaranteed-exact palette size and panel size for the
    /// device actually connected. Every field can change under a running link;
    /// feed it to [`crate::meter::Meter::set_limits`] so the studio's meters
    /// measure against the real device rather than the defaults.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.link.limits()
    }

    /// The link, for anything not in [`PanelStatus`].
    #[must_use]
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// What became of the last frame that reached the wire: codec, bytes,
    /// exactness and the sequence number the panel will report it under.
    /// `None` until one has.
    #[must_use]
    pub fn last_sent(&self) -> Option<Sent> {
        self.last
    }

    /// Everything worth putting on a status strip, in one read.
    #[must_use]
    pub fn status(&self) -> PanelStatus {
        let s = self.link.stats();
        let lim = self.link.limits();
        let last = match self.last {
            Some(Sent::Frame { codec, bytes, exact, .. }) => Some((codec, bytes as u32, exact)),
            _ => None,
        };
        PanelStatus {
            target: self.label.clone(),
            state: state_name(self.link.state()),
            connected: self.link.state().is_up(),
            device: self.link.device().map(|d| format!("{} at {}", d.instance, d.frame)),
            fps: lim.fps,
            budget: lim.budget as u32,
            exact_palette: lim.exact_palette as u32,
            frames_offered: s.frames_offered,
            frames_sent: s.frames_sent,
            frames_coalesced: s.frames_coalesced,
            frames_dropped: s.frames_dropped,
            indexed_exact: s.indexed_exact,
            indexed_fallback: s.indexed_fallback,
            codec: last.map(|l| l.0),
            codec_name: last.map(|l| screeny::codec_name(l.0)),
            bytes: last.map(|l| l.1),
            exact: last.map(|l| l.2),
            last_error: s.last_error.clone(),
        }
    }

    /// Send `FINAL` and let the panel go at once, rather than waiting out its
    /// stream timeout. Dropping the output does the same thing.
    pub fn close(&mut self) {
        self.link.close();
    }
}

impl Output for SenderOutput {
    fn send(&mut self, frame: &WireFrame) -> io::Result<()> {
        let px = match &frame.indexed {
            // The preferred path: palette and indices go on the wire exactly
            // as the piece made them, up to 32 colours whatever the indices,
            // and up to 256 when they compress.
            Some((palette, indices)) => Pixels::indexed(palette, indices),
            // A piece that renders continuous colour: the chooser quantises,
            // which is the best that fits.
            None => Pixels::rgb(&frame.rgb),
        };
        // The only errors are ours - a frame of the wrong size, or an index
        // outside its palette. The network cannot get here.
        let sent = self.link.send(px)?;
        // A coalesced or dropped frame leaves the last *sent* one standing:
        // the status strip should keep showing what the panel has, not blank
        // out every other frame at 60 into 30.
        if sent.is_sent() {
            self.last = Some(sent);
        }
        Ok(())
    }
}

fn label_of(t: &Target) -> String {
    match (&t.addr, &t.name) {
        (Some(a), _) => a.to_string(),
        (None, Some(n)) => n.clone(),
        (None, None) => "the first panel found".into(),
    }
}

fn state_name(s: LinkState) -> &'static str {
    match s {
        LinkState::Up => "up",
        LinkState::Connecting => "connecting",
        LinkState::Waiting => "waiting",
        LinkState::Closed => "closed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_an_address_or_a_name() {
        let a = target_for("127.0.0.1:49374");
        assert_eq!(a.addr.map(|a| a.to_string()).as_deref(), Some("127.0.0.1:49374"));
        assert!(a.name.is_none());

        let b = target_for("192.0.2.7");
        assert_eq!(b.addr.map(|a| a.port()), Some(screeny_proto::DEFAULT_FRAME_PORT));

        let c = target_for(" screeny-4a00a4 ");
        assert!(c.addr.is_none());
        assert_eq!(c.name.as_deref(), Some("screeny-4a00a4"));
    }

    /// A deferred link with nowhere to go is a run of counters, never an
    /// error: the property the render loop depends on.
    #[test]
    fn a_panel_that_is_not_there_is_not_an_error() {
        // Port 1 on loopback: nothing is listening and nothing can be.
        let mut out = SenderOutput::deferred(target_for("127.0.0.1:1"));
        let frame = WireFrame { rgb: vec![0; crate::frame::N * 3], indexed: None };
        for _ in 0..5 {
            out.send(&frame).expect("the network cannot fail a send");
        }
        let st = out.status();
        assert_eq!(st.frames_offered, 5);
        assert_eq!(st.frames_sent, 0);
        assert!(!st.connected);
        assert_eq!(st.target, "127.0.0.1:1");
    }
}
