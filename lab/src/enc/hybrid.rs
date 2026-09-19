//! Per-frame mode switching.
//!
//! The sender has a whole host CPU and only 2048 pixels to think about, so it
//! can simply encode the frame several ways, decode each one, score them, and
//! send the winner. The mode byte already on the wire tells the firmware which
//! decoder to run, so this costs the device nothing.
//!
//! Two details matter:
//!
//! * **Hysteresis.** Switching codec mid-scene changes the *character* of the
//!   error (block edges vs dither noise) and the eye notices that even when
//!   the magnitude is identical. A challenger has to be `MARGIN` better than
//!   the incumbent to take over.
//! * **Statelessness.** The choice is per frame and self-describing. Losing a
//!   packet loses exactly one frame; the next frame decodes on its own.

use super::pal::{emit_pal5, map};
use super::quant;
use super::{lz, Codec, Dither, EncCtx};
use crate::frame::{Frame, NBYTES};
use crate::metrics::mean_de;
use crate::panel::TEMPORAL;

/// A challenger must beat the incumbent mode by this fraction to be chosen.
const MARGIN: f64 = 0.92;

pub struct Hybrid {
    blocks: Vec<Box<dyn Codec>>,
}

impl Hybrid {
    /// Candidates are deliberately limited to codecs that store colour at 8
    /// bits per channel or in a full-precision palette.
    ///
    /// `blk42` and `cc4` (RGB444 endpoints) score well against today's 6-bit
    /// panel and *badly* against the same panel with device-side temporal
    /// dithering -- the panel's coarseness is currently hiding their error.
    /// Picking them would mean the picture gets worse when the firmware gets
    /// better, so they are not on the menu. For the same reason selection is
    /// scored against `panel::TEMPORAL`, not the panel we have today.
    pub fn new() -> Self {
        Hybrid {
            blocks: vec![Box::new(super::block::BlockDual)],
        }
    }
}

impl Default for Hybrid {
    fn default() -> Self {
        Self::new()
    }
}

fn decode_to_frame(p: &[u8]) -> Option<Frame> {
    let mut f = Frame::black();
    let dst: &mut [u8; NBYTES] = &mut f.px;
    crate::dec::decode(p, dst).ok()?;
    Some(f)
}

impl Codec for Hybrid {
    fn name(&self) -> &'static str {
        "hybrid"
    }
    fn note(&self) -> &'static str {
        "per-frame pick of {pal-lz ladder, pal5+dither, bc1-dual} by dE against a temporally dithered panel"
    }
    fn encode(&self, f: &Frame, budget: usize, ctx: &mut EncCtx) -> Vec<u8> {
        let mut cands: Vec<Vec<u8>> = Vec::new();

        // The variable-rate ladder; already guaranteed to fit.
        cands.push(lz::ladder(f, budget, ctx.frame_idx));

        // Dithered 32-colour palette, temporally seeded.
        let pal = quant::build(f, 32, ctx.prev_pal.as_deref());
        let idx = map(&pal, f, Dither::Ordered, ctx.frame_idx);
        cands.push(emit_pal5(&pal, &idx));

        for b in &self.blocks {
            cands.push(b.as_ref().encode(f, budget, ctx));
        }

        let mut best: Option<(f64, usize)> = None;
        for (i, c) in cands.iter().enumerate() {
            if c.len() > budget {
                continue;
            }
            let Some(d) = decode_to_frame(c) else { continue };
            let mut score = mean_de(&TEMPORAL, f, &d);
            if Some(c[0]) == ctx.prev_mode {
                score *= MARGIN; // incumbent advantage
            }
            if best.is_none() || score < best.unwrap().0 {
                best = Some((score, i));
            }
        }
        let pick = best.map(|b| b.1).unwrap_or(0);
        let out = cands.swap_remove(pick);
        ctx.prev_mode = Some(out[0]);
        ctx.prev_pal = Some(pal.srgb);
        out
    }
}
