//! The patches. To add one: write a module with a `DEF`, list it in `ALL`.

use crate::patch::PatchDef;

pub(crate) mod bats;
pub(crate) mod clocks;
pub(crate) mod flock;
#[cfg(feature = "gpu")]
pub(crate) mod ghosts;
// Card 321: leaves became a mesh-based GPU patch (real lighting, a real depth
// buffer, heavy supersampling - the owner's direction was to spend the GPU on
// the picture, not avoid it), so it needs the adapter exactly as knot,
// lattice and overland do, and is gated the same way.
#[cfg(feature = "gpu")]
pub(crate) mod leaves;
#[cfg(feature = "gpu")]
mod knot;
#[cfg(feature = "gpu")]
mod lattice;
mod metaballs;
#[cfg(feature = "gpu")]
mod overland;
pub(crate) mod vesta;

/// Card 178 took two out of this list, at the author's word ("let's also kill
/// the plasma and test card patches - they're not interesting"):
///
/// - **`plasma`**, which is simply gone.
/// - **`testcard`**, which was never art: it was a chart for judging banding
///   and the panel model. What it was for survives as
///   `crates/art/tests/dark_ramp.rs` - card 102's acceptance, with the drawing
///   beside the tests that read it - so nothing that ships draws it and
///   nothing a person can play is a measuring instrument.
///
/// Two more went at the owner's word ("let's kill the thing &
/// skeletons patches - they require professional animation work that we're
/// just not going to have time for"): **`skeletons`** (card 328) and
/// **`thing`** (cards 333-337), with `thing`'s clip assets and
/// `tools/thing-capture`. Both are in git history.
pub static ALL: &[PatchDef] = &[
    clocks::DEF,
    clocks::dials::DEF,
    vesta::DEF,
    metaballs::DEF,
    flock::DEF,
    bats::DEF,
    #[cfg(feature = "gpu")]
    ghosts::DEF,
    #[cfg(feature = "gpu")]
    leaves::DEF,
    #[cfg(feature = "gpu")]
    overland::DEF,
    #[cfg(feature = "gpu")]
    lattice::DEF,
    #[cfg(feature = "gpu")]
    knot::DEF,
];

/// What a patch needs from the machine to draw (card 145, card 357).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// Draws on the CPU; plays anywhere.
    Nothing,
    /// A wgpu adapter, any kind: a software rasteriser (llvmpipe) is fast
    /// enough.
    Adapter,
    /// A hardware adapter. Under a software rasteriser these cost a core or
    /// more and still drop frames (card 357: overland 320 of 600 frames at
    /// 322% CPU, ghosts 8, on a Raspberry Pi 4).
    Hardware,
}

/// The patches that are behind the `gpu` feature, with what each needs. Built
/// from the same `cfg`s as `ALL`, so it cannot drift from it; in a build
/// without the feature it is empty, which is the truth for that build.
static GPU_PATCHES: &[(&str, Need)] = &[
    #[cfg(feature = "gpu")]
    (ghosts::DEF.id, Need::Hardware),
    #[cfg(feature = "gpu")]
    (leaves::DEF.id, Need::Adapter),
    #[cfg(feature = "gpu")]
    (overland::DEF.id, Need::Hardware),
    #[cfg(feature = "gpu")]
    (lattice::DEF.id, Need::Adapter),
    #[cfg(feature = "gpu")]
    (knot::DEF.id, Need::Adapter),
];

/// What this patch needs. Anything not listed draws on the CPU.
#[must_use]
pub fn need(id: &str) -> Need {
    GPU_PATCHES.iter().find(|(i, _)| *i == id).map_or(Need::Nothing, |(_, n)| *n)
}

/// The one rule for "can this machine play this patch": every caller (startup
/// line, page bootstrap, Home Assistant's patch list) asks here. `Err` carries
/// the reason, in words a person can read.
///
/// # Errors
/// When the patch needs more than `gpu` has: any adapter, or a hardware one.
pub fn playable(id: &str, gpu: &crate::GpuStatus) -> Result<(), String> {
    match need(id) {
        Need::Nothing => Ok(()),
        Need::Adapter if gpu.available => Ok(()),
        Need::Adapter => Err(gpu.line()),
        Need::Hardware if gpu.available && !gpu.software => Ok(()),
        Need::Hardware if gpu.available => Err(format!(
            "a graphics card is needed; this machine draws in software ({})",
            gpu.adapter.split(" (").next().unwrap_or(&gpu.adapter)
        )),
        Need::Hardware => Err(gpu.line()),
    }
}

/// The ids this machine cannot play, in list order.
#[must_use]
pub fn blocked(gpu: &crate::GpuStatus) -> Vec<&'static str> {
    GPU_PATCHES.iter().filter(|(id, _)| playable(id, gpu).is_err()).map(|(id, _)| *id).collect()
}

#[cfg(test)]
mod tests {
    use super::{need, Need, ALL};
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
        for def in ALL.iter().filter(|d| need(d.id) == Need::Nothing) {
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

    /// Card 357: the one rule, over the three adapter states and every patch.
    #[test]
    fn playable_follows_what_each_patch_needs_over_every_adapter_state() {
        use super::{blocked, playable};
        use crate::GpuStatus;
        let none = GpuStatus { error: Some("no GPU adapter: nothing".into()), ..GpuStatus::default() };
        let software = GpuStatus {
            available: true,
            adapter: "llvmpipe (LLVM 19.1.7, 128 bits)".into(),
            backend: "Vulkan".into(),
            software: true,
            error: None,
        };
        let hardware = GpuStatus { software: false, adapter: "Apple M3".into(), backend: "Metal".into(), ..software.clone() };
        for def in ALL {
            let n = need(def.id);
            assert!(playable(def.id, &hardware).is_ok(), "{} on hardware", def.id);
            assert_eq!(playable(def.id, &none).is_ok(), n == Need::Nothing, "{} with no adapter", def.id);
            assert_eq!(playable(def.id, &software).is_ok(), n != Need::Hardware, "{} on software", def.id);
        }
        assert!(blocked(&hardware).is_empty());
        if cfg!(feature = "gpu") {
            assert_eq!(blocked(&software), vec!["ghosts", "overland"]);
            assert_eq!(blocked(&none).len(), 5);
            let why = playable("overland", &software).unwrap_err();
            assert!(why.contains("software (llvmpipe)"), "{why}");
            for id in ["leaves", "lattice", "knot"] {
                assert_eq!(need(id), Need::Adapter);
            }
        } else {
            assert!(blocked(&none).is_empty());
        }
        assert_eq!(software.line(), "gpu llvmpipe (LLVM 19.1.7, 128 bits) (Vulkan, software)");
    }
}
