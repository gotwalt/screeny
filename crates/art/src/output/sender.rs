//! Frames to a real panel, over the wire, exactly when the patch made them
//! exactly.
//!
//! This is the whole of the network side of the art system, and it is thin on
//! purpose: `screeny::Link` (card 011) was shaped for this trait. It never
//! sleeps, so the art system keeps its own clock (`screeny_art::FPS`, 30); it
//! drops frames that arrive before the panel's next slot and says so
//! (`Sent::Coalesced`) - which since card 161 should be nothing at all; and it
//! cannot fail because of the network, so a panel that reboots, moves address
//! or is simply off is a counter here rather than an `io::Error` a render loop
//! has to decide what to do about.
//!
//! Behind the `sender` feature, which is **on by default** (card 112): the
//! wire acceptance in `tests/sender.rs` is the only check that indexed frames
//! reach a panel pixel-exact, and it must run in a plain `cargo test`.
//! `--no-default-features` builds `screeny-art` with no network stack at all,
//! for a runner that only pipes or snapshots.

use crate::frame::WireFrame;
use crate::output::Output;
use screeny::{Device, Limits, Link, LinkConfig, LinkState, Pixels, Sent, Target};
use serde::Serialize;
use std::io;

/// Turn `--to <name-or-addr>` into a [`Target`].
///
/// One line, because the grammar is [`Target::parse`]'s and lives in
/// `screeny` so that the CLI, this crate and the studio cannot disagree about
/// it (card 146). In short: `10.0.0.5`, `10.0.0.5:49374` and `[::1]:49374`
/// skip discovery entirely; a name with a dot or a port in it -
/// `host.docker.internal:49374` - is looked up with the system resolver; a
/// bare name is a DNS-SD instance name, which is the better answer for a
/// device on DHCP because the link re-resolves it on every reconnect and so
/// follows the device across a lease.
#[must_use]
pub fn target_for(to: &str) -> Target {
    Target::parse(to)
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
    /// Frames folded away by the cadence ceiling. Card 161 made this **zero in
    /// steady state**: a patch is rendered at the panel's own rate, so there is
    /// nothing to fold. It moves again only when the sender steps its rate down
    /// under sustained loss (spec 6.9).
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

    /// As [`SenderOutput::attach`], but without waiting for the handshake.
    ///
    /// What the studio's device players want (card 106): the registry has
    /// resolved the device, so the link is pinned to those exact two ports and
    /// never browses - but a panel that is switched off at boot is not a
    /// failure, it is Tuesday. Never fails.
    #[must_use]
    pub fn attach_deferred(device: Device, cfg: LinkConfig) -> Self {
        let label = device.label();
        SenderOutput { link: Link::attach_deferred(device, cfg), label, last: None }
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
    /// between patches, while one is loading, or when the studio is paused.
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
            // as the patch made them, up to 32 colours whatever the indices,
            // and up to 256 when they compress.
            Some((palette, indices)) => Pixels::indexed(palette, indices),
            // A patch that renders continuous colour: the chooser quantises,
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
    t.label()
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
    fn a_target_is_an_address_a_host_name_or_an_instance_name() {
        let a = target_for("127.0.0.1:49374");
        assert_eq!(a.addr.map(|a| a.to_string()).as_deref(), Some("127.0.0.1:49374"));
        assert!(a.name.is_none());

        let b = target_for("192.0.2.7");
        assert_eq!(b.addr.map(|a| a.port()), Some(screeny_proto::DEFAULT_FRAME_PORT));

        let c = target_for(" screeny-4a00a4 ");
        assert!(c.addr.is_none() && c.host.is_none());
        assert_eq!(c.name.as_deref(), Some("screeny-4a00a4"));

        // Card 146: the third shape. `{"to":"host.docker.internal:49374"}`
        // used to be browsed for as an instance name and reported as a
        // missing panel; it is a name for the system resolver.
        let d = target_for("host.docker.internal:49374");
        assert_eq!(d.host.as_deref(), Some("host.docker.internal"));
        assert_eq!(d.port, Some(49374));
        assert!(d.addr.is_none() && d.name.is_none());
        assert_eq!(label_of(&d), "host.docker.internal:49374");
    }

    /// A name that resolves to nothing is a counter and a `last_error` that
    /// names the name, not an error out of the render loop, and not "no
    /// screeny device found".
    #[test]
    fn a_hostname_that_resolves_to_nothing_is_reported_as_itself() {
        // RFC 2606 reserves `.invalid`, so this can never reach anything.
        let mut out = SenderOutput::deferred(target_for("nothing-here.invalid:49374"));
        let frame = WireFrame { rgb: vec![0; crate::frame::N * 3], indexed: None };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            out.send(&frame).expect("the network cannot fail a send");
            if out.status().last_error.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let st = out.status();
        assert_eq!(st.target, "nothing-here.invalid:49374");
        assert_eq!(st.frames_sent, 0);
        let err = st.last_error.expect("the lookup failed and said so");
        assert!(err.contains("nothing-here.invalid:49374"), "{err}");
        assert!(!err.contains("no screeny device found"), "{err}");
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
