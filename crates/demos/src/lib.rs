//! Pure renderers for the screeny panel, plus the preview that shows what the
//! panel will make of them. No networking lives here (see
//! `docs/design/architecture.md`): a piece is `fn(t) -> Frame`, and the
//! `screeny` CLI wraps it.
//!
//! Read `docs/design/generative-art-brief.md` before changing any of the art.

pub mod clock;
pub mod color;
pub mod dither;
pub mod font;
pub mod fractal;
pub mod frame;
pub mod panel;
pub mod preview;
pub mod stats;
pub mod testcard;

pub use frame::{Frame, Indexed, LinBuf, Piece, H, NPIX, W};
pub use panel::Panel;
