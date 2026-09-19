//! screeny frame-encoding lab (card 002).
//!
//! `dec` is the constrained half: no_std, no alloc, integer only. Everything
//! else is host-side and may be as expensive as it likes.

pub mod color;
pub mod content;
pub mod dec;
pub mod enc;
pub mod font;
pub mod frame;
pub mod metrics;
pub mod panel;
pub mod sheet;

/// One UDP datagram of 1472 payload bytes, minus the 8-byte transport header
/// card 003 settled on, leaves this for the codec payload. The codec's own
/// mode byte is included in this budget.
pub const BUDGET: usize = 1464;
