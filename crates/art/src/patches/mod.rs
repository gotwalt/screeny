//! The patches. To add one: write a module with a `DEF`, list it in `ALL`.

use crate::patch::PatchDef;

pub(crate) mod clocks;
pub(crate) mod flock;
#[cfg(feature = "gpu")]
mod knot;
#[cfg(feature = "gpu")]
mod lattice;
mod metaballs;
#[cfg(feature = "gpu")]
mod overland;
pub(crate) mod vesta;

/// Card 178 took two out of this list, at the owner's word ("let's also kill
/// the plasma and test card patches - they're not interesting"):
///
/// - **`plasma`**, which is simply gone.
/// - **`testcard`**, which was never art: it was a chart for judging banding
///   and the panel model. What it was for survives as
///   `crates/art/tests/dark_ramp.rs` - card 102's acceptance, with the drawing
///   beside the tests that read it - so nothing that ships draws it and
///   nothing a person can play is a measuring instrument.
pub static ALL: &[PatchDef] = &[
    clocks::DEF,
    clocks::dials::DEF,
    vesta::DEF,
    metaballs::DEF,
    flock::DEF,
    #[cfg(feature = "gpu")]
    overland::DEF,
    #[cfg(feature = "gpu")]
    lattice::DEF,
    #[cfg(feature = "gpu")]
    knot::DEF,
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
    ///
    /// It was `testcard` until card 178; it is `vesta` now, which is a better
    /// subject anyway because it is a patch a person plays. The two clocks
    /// also say `seeded: false`, and for them the claim is narrower and is not
    /// this: their seed picks the *choreography*, so two seeds draw different
    /// pictures while a dance is running and the same one once it has landed
    /// on the minute (`tests/pinned_time.rs` is where that is stated).
    /// `vesta` is the one that never reads the number at all.
    #[test]
    fn vesta_is_the_same_face_whatever_the_seed() {
        let def = crate::patch::find("vesta").expect("vesta is in every build");
        assert!(!def.seeded);
        assert_eq!(frame_of(def, 1), frame_of(def, 999_983));
    }
}
