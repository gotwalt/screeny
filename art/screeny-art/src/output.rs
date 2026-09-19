//! Where finished frames go. The sender's interface is not final, so this is
//! deliberately the smallest thing that covers both hand-over formats in the
//! brief: every `WireFrame` carries raw RGB, and the palette + indices too when
//! the piece rendered indexed. A future `SenderOutput` plugs in here.

use crate::frame::WireFrame;
use std::io::{self, Write};

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
