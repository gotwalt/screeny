//! Colour maths. Everything that blends, filters or dithers does it in linear
//! light; sRGB8 exists only at the crate's edges (frames out, preview PNGs).
//!
//! One implementation, in [`screeny_panel::color`] (card 016). It used to be
//! copied here from `lab/src/color.rs` (card 002) "so the two agree exactly",
//! which is a promise a copy cannot keep.

pub use screeny_panel::color::*;
