//! The pieces. To add one: write a module with a `DEF`, list it in `ALL`.

use crate::piece::PieceDef;

#[cfg(feature = "gpu")]
mod knot;
#[cfg(feature = "gpu")]
mod lattice;
mod metaballs;
mod plasma;
mod testcard;

pub static ALL: &[PieceDef] = &[
    plasma::DEF,
    metaballs::DEF,
    #[cfg(feature = "gpu")]
    lattice::DEF,
    #[cfg(feature = "gpu")]
    knot::DEF,
    testcard::DEF,
];
