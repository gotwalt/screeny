//! What the LED panel does to a colour.
//!
//! Two things that everything host-side needs and that used to exist three
//! times over (card 016):
//!
//! * [`color`] - the sRGB EOTF and its inverse, Oklab and OKLCH, and the small
//!   fast paths the chooser lives on.
//! * [`model`] - the panel's sRGB8 -> emitted-light transfer function, and the
//!   brightness lookup a receiver applies on top of it.
//!
//! The sender scores codecs against this model, the demos design their
//! palettes against it, and the simulator draws its dots through it. They have
//! to be the same function or the picture the preview shows is not the picture
//! the panel will make.
//!
//! Nothing here ships to the device: the firmware's equivalent is an integer
//! gamma LUT with no `f32` in it (`firmware/src/gamma.rs`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(
    // Colour-space code is full of `r, g, b`, `l, m, s` and `#[inline(always)]`
    // inner loops, and reads better for it.
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::inline_always,
    clippy::needless_range_loop
)]

pub mod color;
pub mod model;

pub use model::{Lut, Panel, DEEP, DIM, DIMMED, NOMINAL, TEMPORAL};
