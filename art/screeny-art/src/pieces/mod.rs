//! The pieces. To add one: write a module with a `DEF`, list it in `ALL`.

use crate::piece::PieceDef;

mod clocks;
#[cfg(feature = "gpu")]
mod knot;
#[cfg(feature = "gpu")]
mod lattice;
mod metaballs;
#[cfg(feature = "gpu")]
mod overland;
mod plasma;
mod testcard;

pub static ALL: &[PieceDef] = &[
    clocks::DEF,
    plasma::DEF,
    metaballs::DEF,
    #[cfg(feature = "gpu")]
    overland::DEF,
    #[cfg(feature = "gpu")]
    lattice::DEF,
    #[cfg(feature = "gpu")]
    knot::DEF,
    testcard::DEF,
];
