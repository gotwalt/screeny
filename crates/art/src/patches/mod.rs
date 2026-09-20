//! The patches. To add one: write a module with a `DEF`, list it in `ALL`.

use crate::patch::PatchDef;

pub(crate) mod clocks;
#[cfg(feature = "gpu")]
mod knot;
#[cfg(feature = "gpu")]
mod lattice;
mod metaballs;
#[cfg(feature = "gpu")]
mod overland;
mod plasma;
mod testcard;

pub static ALL: &[PatchDef] = &[
    clocks::DEF,
    clocks::dials::DEF,
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

/// The patches that need a graphics adapter (card 145).
///
/// They are exactly the ones behind the `gpu` feature, so this list is built
/// from the same `cfg`s as `ALL` and cannot drift from it. Without an adapter
/// they render black and say so only on stderr, so the studio marks them on
/// the page rather than letting a black picture pass silently. In a build
/// without the feature they are not here at all and this is empty, which is
/// the truth for that build.
pub static NEEDS_GPU: &[&str] = &[
    #[cfg(feature = "gpu")]
    overland::DEF.id,
    #[cfg(feature = "gpu")]
    lattice::DEF.id,
    #[cfg(feature = "gpu")]
    knot::DEF.id,
];

/// True when this patch cannot draw without a graphics adapter.
#[must_use]
pub fn needs_gpu(id: &str) -> bool {
    NEEDS_GPU.contains(&id)
}
