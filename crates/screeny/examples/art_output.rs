//! What the generative art system's `Output` impl will look like.
//!
//! The art system (`art/screeny-art`, on its own branch until it is merged)
//! ends every frame in a `WireFrame` and hands it to an `Output`:
//!
//! ```text
//! pub trait Output {
//!     fn send(&mut self, frame: &WireFrame) -> io::Result<()>;
//! }
//!
//! pub struct WireFrame {
//!     pub rgb: Vec<u8>,                                 // N * 3, always present
//!     pub indexed: Option<(Vec<[u8; 3]>, Vec<u8>)>,     // when the piece rendered indexed
//! }
//! ```
//!
//! Both are restated here as stand-ins so this file compiles on its own; the
//! real ones live in `art/screeny-art/src/output.rs` and `frame.rs` and are
//! not touched by this crate. `SenderOutput` below is the whole of the work
//! that merge will need. **It is 15 lines.**
//!
//! Three things it relies on, all of them this crate's job rather than
//! theirs:
//!
//! * `Pixels` takes slices, so `&frame.rgb` and `&indices` go straight in
//!   with no conversion from `Vec<u8>` to `[u8; 6144]`.
//! * `screeny::Error` converts to `io::Error`, so `?` is enough and their
//!   trait signature does not have to change.
//! * `Link::send` cannot fail because of the network. A panel that reboots
//!   mid-piece never becomes an `io::Error` their paced loop has to decide
//!   what to do about - the frames are dropped, counted, and the link comes
//!   back by itself.
//!
//! ```sh
//! cargo run --release -p screeny --example art_output -- --addr 127.0.0.1:49374
//! ```

use std::io;
use std::time::Duration;

use screeny::{Link, LinkConfig, Pixels, Target};

// ---------------------------------------------------------------------------
// Stand-ins for the art system's own types (do not edit art/ from here)
// ---------------------------------------------------------------------------

const N: usize = 64 * 32;

/// Mirrors `screeny_art::frame::WireFrame`.
pub struct WireFrame {
    /// `N * 3` bytes, row-major sRGB R,G,B. Always present.
    pub rgb: Vec<u8>,
    /// Present when the piece rendered indexed: sRGB palette and `N` indices.
    pub indexed: Option<(Vec<[u8; 3]>, Vec<u8>)>,
}

/// Mirrors `screeny_art::output::Output`.
pub trait Output {
    /// Called by the art system's own paced loop, at its own rate.
    fn send(&mut self, frame: &WireFrame) -> io::Result<()>;
}

// ---------------------------------------------------------------------------
// The impl
// ---------------------------------------------------------------------------

/// Frames to a real panel, over the wire, exactly when the piece made them
/// exactly.
pub struct SenderOutput {
    link: Link,
}

impl SenderOutput {
    /// Connect. `Target::default()` browses `_screeny._udp`;
    /// `Target { addr: Some(a), .. }` skips discovery.
    ///
    /// # Errors
    ///
    /// If the panel cannot be found or does not answer. Use
    /// [`SenderOutput::deferred`] to start without one.
    pub fn open(target: Target) -> io::Result<Self> {
        Ok(SenderOutput {
            link: Link::open(target, LinkConfig::default())?,
        })
    }

    /// Start without a panel and pick one up whenever it appears. What a
    /// server process wants: a panel that is off at boot is not a different
    /// situation from one that is unplugged an hour later.
    #[must_use]
    pub fn deferred(target: Target) -> Self {
        SenderOutput {
            link: Link::open_deferred(target, LinkConfig::default()),
        }
    }

    /// The rate the panel is keeping up with, for the art system's limiter
    /// and its "now playing" readout. Follows spec 6.9's adaptation.
    #[must_use]
    pub fn fps(&self) -> f64 {
        self.link.fps()
    }

    /// The link, for anything else worth showing in an inspector: state,
    /// device label, lifetime counters, the device's own telemetry.
    #[must_use]
    pub fn link(&self) -> &Link {
        &self.link
    }
}

impl Output for SenderOutput {
    fn send(&mut self, frame: &WireFrame) -> io::Result<()> {
        let px = match &frame.indexed {
            // The preferred path: palette and indices go on the wire exactly
            // as the piece made them, up to 32 colours, whatever the indices.
            Some((palette, indices)) => Pixels::indexed(palette, indices),
            // More than 32 colours, or a piece that renders continuous
            // colour: the chooser quantises, which is the best that fits.
            None => Pixels::rgb(&frame.rgb),
        };
        self.link.send(px)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// A loop shaped like theirs, so this example does something
// ---------------------------------------------------------------------------

/// Stands in for `Pipeline::process`: a 24-colour indexed frame, exact.
fn render(t: f64) -> WireFrame {
    const K: usize = 24;
    let palette: Vec<[u8; 3]> = (0..K)
        .map(|i| {
            let a = (i as f64 / K as f64 + t * 0.2) * std::f64::consts::TAU;
            let f = |o: f64| (((a + o).sin() * 0.5 + 0.5) * 200.0 + 30.0) as u8;
            [f(0.0), f(2.1), f(4.2)]
        })
        .collect();
    let indices: Vec<u8> = (0..N)
        .map(|p| {
            let (x, y) = ((p % 64) as f64, (p / 64) as f64);
            let r = ((x - 31.5).powi(2) + (y - 15.5).powi(2)).sqrt();
            ((r + t * 6.0) as usize % K) as u8
        })
        .collect();
    // The art system always fills `rgb` as well, so an output that cannot
    // take indices still works.
    let mut rgb = Vec::with_capacity(N * 3);
    for &i in &indices {
        rgb.extend_from_slice(&palette[i as usize]);
    }
    WireFrame {
        rgb,
        indexed: Some((palette, indices)),
    }
}

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut target = Target::default();
    while let Some(a) = args.next() {
        if a == "--addr" {
            let v = args.next().unwrap_or_default();
            let v = if v.contains(':') { v } else { format!("{v}:49374") };
            target.addr = Some(v.parse().expect("--addr IP[:PORT]"));
        } else {
            eprintln!("usage: art_output [--addr IP[:PORT]]");
            std::process::exit(2);
        }
    }

    // Their loop owns the clock; ours never sleeps on their behalf.
    let mut out = SenderOutput::deferred(target);
    let start = std::time::Instant::now();
    let mut n = 0u64;
    loop {
        out.send(&render(start.elapsed().as_secs_f64()))?;
        n += 1;
        // The art system renders at 60; the link decimates to whatever the
        // panel is keeping up with, so this is not 60 packets a second.
        screeny::sender::sleep_until(start + Duration::from_micros(16_667 * n));
        if n.is_multiple_of(120) {
            let s = out.link().stats();
            println!(
                "{:?} {} offered / {} sent / {} coalesced / {} dropped, {} exact, {:.0} fps",
                out.link().state(),
                s.frames_offered,
                s.frames_sent,
                s.frames_coalesced,
                s.frames_dropped,
                s.indexed_exact,
                out.fps(),
            );
        }
    }
}
