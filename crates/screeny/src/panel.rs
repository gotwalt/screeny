//! What the LED panel actually emits.
//!
//! One implementation, in [`screeny_panel::model`] (card 016), shared with
//! `crates/demos` and `crates/sim`. The reasoning is in that module; the short
//! version is that codec error has to be judged after the panel's transfer
//! function, because error the panel cannot show is not error.
//!
//! This module is the path the chooser, the CLI and the art already use.

pub use screeny_panel::model::{Lut, Panel, DEEP, DEVICE, DIM, DIMMED, DITHER_PHASES, NOMINAL, TEMPORAL};
