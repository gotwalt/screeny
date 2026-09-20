//! Where finished frames go.
//!
//! Every `WireFrame` carries raw RGB, and the palette + indices too when the
//! piece rendered indexed, so an output can take whichever it can use.
//!
//! * [`PipeOutput`] writes the RGB: 6144 bytes a frame on a pipe, for anything
//!   that is not this process.
//! * [`SenderOutput`] is the real one - frames to a panel over UDP through
//!   `screeny`'s `Link`, indexed frames exactly - and lives behind the
//!   `sender` feature, which is on by default (card 112);
//!   `--no-default-features` is the build with no network stack.

use crate::frame::WireFrame;
use std::io::{self, Write};

#[cfg(feature = "sender")]
mod sender;
#[cfg(feature = "sender")]
pub use sender::{target_for, PanelStatus, SenderOutput};

pub trait Output {
    fn send(&mut self, frame: &WireFrame) -> io::Result<()>;
}

/// Raw frames on a pipe: 6144 bytes each, row-major sRGB R,G,B, no framing.
pub struct PipeOutput<W: Write>(pub W);

impl<W: Write> Output for PipeOutput<W> {
    fn send(&mut self, frame: &WireFrame) -> io::Result<()> {
        self.0.write_all(&frame.rgb)?;
        self.0.flush()
    }
}
