//! Generative art for the screeny panel: 64x32 RGB LEDs, 6-bit linear, one
//! frame per 1464-byte datagram. See `docs/design/generative-art-brief.md`.
//!
//! Pieces make frames; the pipeline makes them safe and displayable; outputs
//! take them away.
//!
//! Frame costs and the preview are the real encoder's answers, not estimates
//! ([`meter`]). Talking to a device is [`output::SenderOutput`], behind the
//! `sender` feature - on by default (card 112), because sending is what this
//! crate is for. `--no-default-features` turns it off, and then nothing here
//! opens a socket.

pub mod color;
pub mod dither;
pub mod frame;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod limiter;
pub mod meter;
pub mod output;
pub mod palette;
pub mod panel;
pub mod piece;
pub mod pieces;
pub mod pipeline;
pub mod preview;
pub mod rng;
pub mod variety;

pub use color::Rgb;
pub use frame::{Frame, WireFrame, H, N, W};
pub use meter::{Measured, Meter};
pub use piece::{Ctx, Params, Piece, PieceDef};
pub use pipeline::{Pipeline, Settings, Stats};
