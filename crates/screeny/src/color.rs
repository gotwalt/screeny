//! Colour spaces for the encoders and the scorer.
//!
//! One implementation, in [`screeny_panel::color`] (card 016). It used to live
//! here, in `crates/demos` and in `crates/sim`, with the note "`crates/screeny`
//! will eventually own one copy"; it does now, one crate lower down, where the
//! demos can reach it without a dependency cycle.
//!
//! This module is the path the sender, the benches and the art already use.

pub use screeny_panel::color::*;
