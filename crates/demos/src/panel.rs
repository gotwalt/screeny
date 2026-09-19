//! The panel model: what the LEDs actually emit for an sRGB8 frame.
//!
//! One implementation, in [`screeny_panel::model`] (card 016), shared with the
//! sender and the simulator. The panel modulates each channel in **linear
//! light** with a fixed number of steps (64 at the measured 6 bitplanes /
//! 154 Hz; [`DIM`] is the 32-level check every piece also has to pass). See
//! `docs/design/generative-art-brief.md` section 2.1.

pub use screeny_panel::model::{Panel, DIM, NOMINAL};
