//! The screeny host sender.
//!
//! Turns frames into the best picture a 64x32 LED panel can show inside one
//! 1472-byte UDP datagram, thirty times a second.
//! [`docs/design/protocol-v1.md`](../../../docs/design/protocol-v1.md) is
//! normative and [`screeny_proto`] is its executable form; this crate is the
//! host half - encoders, the per-frame codec chooser, discovery, and a paced
//! sender that listens to the device's telemetry.
//!
//! ```no_run
//! use std::sync::atomic::AtomicBool;
//! use screeny::{Pattern, Sender, SenderConfig, Target};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let device = Target::default().resolve()?;       // mDNS, or Target::addr
//! let mut sender = Sender::connect(device, SenderConfig::default())?;
//! sender.run(&mut Pattern::Bars, &AtomicBool::new(false))?;
//! # Ok(()) }
//! ```
//!
//! The four pieces, each usable on its own:
//!
//! * [`encode`] - the encoders, the palette ladder and the chooser. Give it a
//!   frame and a byte budget, get back a codec id and a payload that is
//!   guaranteed to fit and guaranteed to decode.
//! * [`discover`] - browse `_screeny._udp`, or probe by broadcast when mDNS is
//!   having one of its days.
//! * [`control`] - the control port: `GET_INFO`, `PING`, telemetry,
//!   brightness, identify, reboot.
//! * [`sender`] - the pacing loop, sequence numbers, `STATS_REQ`, and the
//!   telemetry-driven adaptation in spec 6.9.
//!
//! [`FrameSource`] is the seam renderers plug into: a demo is a pure
//! `fn(t) -> pixels` and knows nothing about codecs or sockets.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::needless_range_loop,
    clippy::module_name_repetitions,
    // Colour-space and codec code is full of `r, g, b`, `l, m, s` and
    // `#[inline(always)]` inner loops, and reads better for it.
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::inline_always,
    clippy::unnested_or_patterns,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::struct_excessive_bools,
    clippy::items_after_statements
)]

pub mod color;
pub mod control;
pub mod device;
pub mod discover;
pub mod encode;
pub mod error;
pub mod frame;
pub mod net;
pub mod panel;
pub mod patterns;
pub mod sender;

pub use control::ControlClient;
pub use device::{Device, DeviceInfo};
pub use discover::Target;
pub use encode::{EncodeConfig, Encoded, Encoder, Profile};
pub use error::{Error, Result};
pub use frame::{FnSource, Frame, FrameSource, FrameTime, RawReader};
pub use panel::Panel;
pub use patterns::Pattern;
pub use sender::{SendStats, Sender, SenderConfig};

/// Re-exported so callers do not have to depend on the wire crate directly.
pub use screeny_proto as proto;

/// A short name for a codec id, for logs and stats.
#[must_use]
pub fn codec_name(id: u8) -> &'static str {
    use screeny_proto::dec::codec;
    match id {
        codec::PAL5 => "pal5",
        codec::PAL8_LZ => "pal8-lz",
        codec::PAL4_LZ => "pal4-lz",
        codec::BC1_DUAL => "bc1-dual",
        codec::SOLID => "solid",
        _ => "?",
    }
}
