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

#[cfg(test)]
mod tests {
    use super::{needs_gpu, ALL};
    use crate::frame::Frame;
    use crate::patch::{Ctx, Params};

    /// One frame of a patch built on `seed`, as comparable pixels.
    fn frame_of(def: &crate::patch::PatchDef, seed: u64) -> Vec<[f32; 3]> {
        let params = Params::defaults(def.params);
        let mut patch = (def.make)(seed);
        // Not the first frame: a patch that eases out of a rest pose looks the
        // same on every seed at t = 0 and has moved apart by a second.
        let mut out = Vec::new();
        for i in 1..=2 {
            let t = f64::from(i);
            let frame = patch.render(&Ctx { t, dt: 1.0, now: 0.0, params: &params });
            let px = match frame {
                Frame::Linear(px) => px,
                Frame::Indexed { palette, indices } => indices.iter().map(|i| palette[*i as usize]).collect(),
            };
            out = px.iter().map(|c| [c.r, c.g, c.b]).collect();
        }
        out
    }

    /// Card 151: `seeded` is a promise to a person - "another one like this" -
    /// and the studio draws a button from it. A patch that claims it must
    /// really look different on another seed.
    ///
    /// CPU patches only: the GPU ones read `u.seed` in their shader (checked by
    /// eye and by the shader source) and a test machine may have no adapter.
    #[test]
    fn a_patch_that_says_it_is_seeded_looks_different_on_another_seed() {
        for def in ALL.iter().filter(|d| !needs_gpu(d.id)) {
            let differs = frame_of(def, 1) != frame_of(def, 999_983);
            if def.seeded {
                assert!(differs, "`{}` says `seeded` but two seeds draw the same picture", def.id);
            }
        }
    }

    /// And the one patch that ignores its seed outright really does.
    #[test]
    fn the_test_card_is_the_same_card_whatever_the_seed() {
        let card = super::testcard::DEF;
        assert!(!card.seeded);
        assert_eq!(frame_of(&card, 1), frame_of(&card, 999_983));
    }
}
