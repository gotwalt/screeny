//! Encoders and the per-frame codec chooser.
//!
//! The sender has a whole host CPU and only 2048 pixels to think about, so it
//! encodes each frame several ways, decodes each candidate **through
//! `screeny_proto`** - the same decoder the firmware runs - scores them
//! against a model of the panel, and sends the winner. The codec id in the
//! header tells the device which decoder to run, so this costs the device
//! nothing and needs no negotiation (spec 4.7, 4.8).
//!
//! ```no_run
//! use screeny::encode::{EncodeConfig, Encoder};
//! use screeny::Frame;
//!
//! let mut enc = Encoder::new(EncodeConfig::default());
//! let frame = Frame::solid([10, 20, 30]);
//! let out = enc.encode(&frame, 1464);
//! assert_eq!(out.codec, screeny_proto::dec::codec::SOLID);
//! ```
//!
//! Two properties hold for every frame and every budget down to
//! [`MIN_BUDGET`]:
//!
//! * the payload never exceeds the budget - the ladder's floor is a
//!   fixed-rate codec, and below even that there is always `SOLID`;
//! * the payload decodes. Every candidate is decoded before it is scored, so
//!   the winner has been round-tripped through the real decoder by
//!   construction.
//!
//! # Hysteresis
//!
//! Switching codec mid-scene changes the *character* of the error - block
//! edges versus dither noise - and the eye notices that even when the
//! magnitude is identical. A challenger has to be
//! [`EncodeConfig::hysteresis`] times the incumbent's score to take over.

pub mod block;
pub mod hist;
pub mod lz;
pub mod pal;
pub mod quant;
pub mod score;

use std::time::{Duration, Instant};

use screeny_proto::dec::{codec, BC1_DUAL_LEN, PAL5_LEN, SOLID_LEN, SUPPORTED_CODECS};
use screeny_proto::{DecodeError, IndexedFrame, Rgb888Frame, NPIX};

use crate::panel::{Panel, TEMPORAL};

use hist::Hist;
use lz::{LzScratch, CHAIN_FAST, CHAIN_FULL};
use pal::Dither;
use quant::{Palette, QuantEffort};
use score::Scorer;

/// The smallest budget the palette ladder can always satisfy: a `PAL5`
/// payload. Below this the encoder falls back to `BC1_DUAL` (1296) and then
/// `SOLID` (3), so it still never overruns, but the picture is no longer the
/// one card 002 measured.
pub const MIN_BUDGET: usize = PAL5_LEN;

/// How hard the chooser works.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Profile {
    /// Everything card 002 measured: full-depth LZ chains, ten Lloyd
    /// iterations, every pixel scored.
    #[default]
    Full,
    /// The documented shortcut set from card 031: shorter LZ chains, fewer
    /// Lloyd iterations, one pixel in four scored. Measurably faster, with the
    /// dE cost recorded in the card log.
    Fast,
}

impl Profile {
    fn quant(self) -> QuantEffort {
        match self {
            Profile::Full => QuantEffort::FULL,
            Profile::Fast => QuantEffort::FAST,
        }
    }
    fn chain(self) -> usize {
        match self {
            Profile::Full => CHAIN_FULL,
            Profile::Fast => CHAIN_FAST,
        }
    }
    fn stride(self) -> usize {
        match self {
            Profile::Full => 1,
            Profile::Fast => 4,
        }
    }
}

/// How to encode.
#[derive(Clone, Debug)]
pub struct EncodeConfig {
    /// Codec ids the encoder may emit. A sender must never send a codec the
    /// device did not advertise (spec 4.7), so this is the device's list.
    pub codecs: Vec<u8>,
    /// Speed/quality profile.
    pub profile: Profile,
    /// Panel model the chooser scores against.
    pub panel: Panel,
    /// Multiplier applied to the incumbent codec's score. Below 1 it is an
    /// advantage; 1.0 disables hysteresis.
    pub hysteresis: f64,
    /// Dither used by the fixed-rate `PAL5` candidate and the ladder's floor.
    pub dither: Dither,
    /// Record per-stage timings in [`FrameStats::stages`]. Off by default;
    /// the benchmark turns it on.
    pub measure_stages: bool,
}

impl Default for EncodeConfig {
    fn default() -> Self {
        EncodeConfig {
            codecs: SUPPORTED_CODECS.to_vec(),
            profile: Profile::default(),
            panel: TEMPORAL,
            hysteresis: 0.92,
            dither: Dither::Ordered,
            measure_stages: false,
        }
    }
}

/// One encoded frame: what goes in the header's `codec` byte, and what goes
/// after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    /// Codec id for the `FRAME` header (spec 4).
    pub codec: u8,
    /// Pixel payload. The codec id is **not** in here - that is the one place
    /// the wire differs from the card 002 lab.
    pub payload: Vec<u8>,
}

/// Where the time went, when [`EncodeConfig::measure_stages`] is set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stages {
    /// Histogram build.
    pub histogram: Duration,
    /// Oklab of the distinct colours.
    pub lab: Duration,
    /// Palette design (median cut + Lloyd), over all ladder rungs.
    pub quantise: Duration,
    /// Palette mapping and dithering.
    pub map: Duration,
    /// LZ compression.
    pub lz: Duration,
    /// `BC1_DUAL` block fitting.
    pub block: Duration,
    /// Decoding the candidates.
    pub decode: Duration,
    /// Scoring the candidates.
    pub scoring: Duration,
}

/// What the encoder did with one frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameStats {
    /// Chosen codec id.
    pub codec: u8,
    /// Payload bytes.
    pub bytes: usize,
    /// Wall-clock time for the whole call.
    pub elapsed: Duration,
    /// Distinct colours in the source frame.
    pub colours: usize,
    /// How many candidates were built and scored. Zero means a shortcut was
    /// taken because the result is exact by construction.
    pub candidates: usize,
    /// True when the payload reproduces the source frame bit for bit.
    pub exact: bool,
    /// Winning candidate's mean Oklab dE, when anything was scored.
    pub score: Option<f64>,
    /// Stage breakdown, if [`EncodeConfig::measure_stages`] was set.
    pub stages: Stages,
}

impl Default for FrameStats {
    fn default() -> Self {
        FrameStats {
            codec: 0,
            bytes: 0,
            elapsed: Duration::ZERO,
            colours: 0,
            candidates: 0,
            exact: false,
            score: None,
            stages: Stages::default(),
        }
    }
}

/// A stopwatch that costs nothing when stage timing is off.
struct Clock {
    on: bool,
    at: Instant,
}

impl Clock {
    fn new(on: bool) -> Self {
        Clock {
            on,
            at: Instant::now(),
        }
    }
    #[inline]
    fn lap(&mut self, slot: &mut Duration) {
        if self.on {
            let now = Instant::now();
            *slot += now - self.at;
            self.at = now;
        }
    }
    #[inline]
    fn mark(&mut self) {
        if self.on {
            self.at = Instant::now();
        }
    }
}

/// The per-frame chooser, plus all the working storage it reuses.
///
/// One `Encoder` per stream: it carries the previous frame's palettes (Lloyd
/// seeds, which cut both flicker and time) and the previous frame's codec
/// (hysteresis).
pub struct Encoder {
    cfg: EncodeConfig,
    hist: Hist,
    lz: LzScratch,
    scorer: Scorer,
    idx: Box<[u8; NPIX]>,
    dec: Box<Rgb888Frame>,
    prev_pal: [Option<Vec<[u8; 3]>>; 5],
    prev_codec: Option<u8>,
    frame_idx: usize,
    stats: FrameStats,
}

/// Palette sizes the ladder uses, smallest first. Index into `prev_pal`.
const LADDER_K: [usize; 5] = [16, 32, 64, 128, 256];

fn pal_slot(k: usize) -> Option<usize> {
    LADDER_K.iter().position(|&x| x == k)
}

impl Encoder {
    /// A new encoder.
    #[must_use]
    pub fn new(cfg: EncodeConfig) -> Self {
        let scorer = Scorer::new(cfg.panel, cfg.profile.stride());
        Encoder {
            hist: Hist::new(),
            lz: LzScratch::new(),
            scorer,
            idx: Box::new([0u8; NPIX]),
            dec: Box::new([0u8; screeny_proto::NBYTES]),
            prev_pal: [None, None, None, None, None],
            prev_codec: None,
            frame_idx: 0,
            stats: FrameStats::default(),
            cfg,
        }
    }

    /// The configuration in use.
    #[must_use]
    pub fn config(&self) -> &EncodeConfig {
        &self.cfg
    }

    /// Restrict the codecs the encoder may emit (spec 4.7: never send one the
    /// device did not advertise). Clears the hysteresis incumbent if it is no
    /// longer allowed.
    pub fn set_codecs(&mut self, codecs: Vec<u8>) {
        self.cfg.codecs = codecs;
        if let Some(c) = self.prev_codec {
            if !self.allows(c) {
                self.prev_codec = None;
            }
        }
    }

    /// Switch profile. Palette seeds survive; the scorer is rebuilt because
    /// its sampling lattice changes.
    pub fn set_profile(&mut self, p: Profile) {
        if self.cfg.profile != p {
            self.cfg.profile = p;
            self.scorer = Scorer::new(self.cfg.panel, p.stride());
        }
    }

    /// What the last [`Encoder::encode`] call did.
    #[must_use]
    pub fn last_stats(&self) -> FrameStats {
        self.stats
    }

    /// Forget the previous frame: palettes, codec, and the dither phase.
    pub fn reset(&mut self) {
        self.prev_pal = [None, None, None, None, None];
        self.prev_codec = None;
        self.frame_idx = 0;
    }

    #[inline]
    fn allows(&self, id: u8) -> bool {
        self.cfg.codecs.contains(&id)
    }

    /// Encode one frame into at most `budget` bytes.
    ///
    /// Never returns a payload longer than `budget` as long as `budget >= 3`,
    /// and never returns a codec outside [`EncodeConfig::codecs`] unless that
    /// list is so restrictive that nothing fits, in which case `SOLID` is
    /// used because a wrong colour beats a dropped frame.
    pub fn encode(&mut self, f: &Rgb888Frame, budget: usize) -> Encoded {
        let t0 = Instant::now();
        self.stats = FrameStats::default();
        let mut clock = Clock::new(self.cfg.measure_stages);

        self.hist.rebuild(f);
        clock.lap(&mut self.stats.stages.histogram);
        let ncol = self.hist.len();
        self.stats.colours = ncol;

        // --- shortcut: one colour ------------------------------------------
        if ncol == 1 && self.allows(codec::SOLID) && budget >= SOLID_LEN {
            let c = self.hist.bins[0].srgb;
            return self.finish(
                Encoded {
                    codec: codec::SOLID,
                    payload: c.to_vec(),
                },
                true,
                None,
                t0,
            );
        }

        // --- the ladder ----------------------------------------------------
        let (ladder, exact) = self.ladder(f, budget, &mut clock);
        if exact {
            // Lossless by construction: nothing can score better than zero, so
            // the other candidates are not built at all. This is the shortcut
            // card 031 asked for, generalised from "<= 16 or <= 32 colours" to
            // "any frame the ladder can carry losslessly".
            return self.finish(ladder, true, Some(0.0), t0);
        }

        // --- the other two candidates --------------------------------------
        self.hist.ensure_lab();
        clock.lap(&mut self.stats.stages.lab);

        let mut cands: Vec<Encoded> = Vec::with_capacity(3);
        cands.push(ladder);

        if self.allows(codec::PAL5) && budget >= PAL5_LEN {
            clock.mark();
            let seed = self.prev_pal[pal_slot(32).unwrap()].clone();
            let pal = quant::build(&self.hist, 32, seed.as_deref(), self.cfg.profile.quant());
            clock.lap(&mut self.stats.stages.quantise);
            pal::map(
                &self.hist,
                &pal,
                self.cfg.dither,
                self.frame_idx,
                &mut self.idx,
            );
            clock.lap(&mut self.stats.stages.map);
            cands.push(Encoded {
                codec: codec::PAL5,
                payload: pal::emit_pal5(&pal, &self.idx),
            });
            self.remember(32, &pal);
        }

        if self.allows(codec::BC1_DUAL) && budget >= BC1_DUAL_LEN {
            clock.mark();
            cands.push(Encoded {
                codec: codec::BC1_DUAL,
                payload: block::encode(f, &self.hist),
            });
            clock.lap(&mut self.stats.stages.block);
        }

        // --- decode, score, pick -------------------------------------------
        clock.mark();
        self.scorer.prepare(f);
        clock.lap(&mut self.stats.stages.scoring);

        let mut best: Option<(f64, usize)> = None;
        let mut scored = 0usize;
        for (i, c) in cands.iter().enumerate() {
            if c.payload.len() > budget {
                continue;
            }
            clock.mark();
            let ok = screeny_proto::decode(c.codec, &c.payload, &mut self.dec).is_ok();
            clock.lap(&mut self.stats.stages.decode);
            if !ok {
                // Cannot happen for a payload this crate built; if it ever
                // does, dropping the candidate is the safe response.
                debug_assert!(ok, "candidate codec {:#04x} did not decode", c.codec);
                continue;
            }
            let mut s = self.scorer.score(&self.dec);
            clock.lap(&mut self.stats.stages.scoring);
            scored += 1;
            if Some(c.codec) == self.prev_codec {
                s *= self.cfg.hysteresis;
            }
            if best.is_none() || s < best.unwrap().0 {
                best = Some((s, i));
            }
        }

        self.stats.candidates = scored;
        let pick = best.map_or(0, |b| b.1);
        let score = best.map(|b| b.0);
        let out = cands.swap_remove(pick);
        self.finish(out, false, score, t0)
    }

    /// Encode a frame that is already in palette form.
    ///
    /// A palette of 16 or fewer colours goes straight out as `PAL4_LZ`, 32 or
    /// fewer as `PAL5`, 256 or fewer as `PAL8_LZ` - all three are exact by
    /// construction, so there is nothing to choose between (spec 4.8). Only
    /// if none of those fit the budget does the frame get expanded and run
    /// through the full chooser.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Corrupt`] if an index falls outside the palette.
    pub fn encode_indexed(
        &mut self,
        f: &IndexedFrame<'_>,
        budget: usize,
    ) -> Result<Encoded, DecodeError> {
        let n = f.palette.len();
        if n == 0 || n > 256 {
            return Err(DecodeError::Corrupt);
        }
        for &i in f.indices {
            if i as usize >= n {
                return Err(DecodeError::Corrupt);
            }
        }
        let t0 = Instant::now();
        self.stats = FrameStats::default();
        self.stats.colours = n;

        let chain = self.cfg.profile.chain();
        if n <= 16 && self.allows(codec::PAL4_LZ) {
            let v = lz::emit_pal4_lz(f.palette, f.indices, &mut self.lz, chain);
            if v.len() <= budget {
                return Ok(self.finish(
                    Encoded {
                        codec: codec::PAL4_LZ,
                        payload: v,
                    },
                    true,
                    Some(0.0),
                    t0,
                ));
            }
        }
        if self.allows(codec::PAL8_LZ) {
            let v = lz::emit_pal8_lz(f.palette, f.indices, &mut self.lz, chain);
            if v.len() <= budget {
                return Ok(self.finish(
                    Encoded {
                        codec: codec::PAL8_LZ,
                        payload: v,
                    },
                    true,
                    Some(0.0),
                    t0,
                ));
            }
        }
        if n <= 32 && self.allows(codec::PAL5) && budget >= PAL5_LEN {
            let mut srgb = f.palette.to_vec();
            srgb.resize(32, [0, 0, 0]);
            let pal = Palette::from_srgb(srgb);
            self.idx.copy_from_slice(f.indices);
            return Ok(self.finish(
                Encoded {
                    codec: codec::PAL5,
                    payload: pal::emit_pal5(&pal, &self.idx),
                },
                true,
                Some(0.0),
                t0,
            ));
        }

        let mut px = Box::new([0u8; screeny_proto::NBYTES]);
        f.expand(&mut px)?;
        Ok(self.encode(&px, budget))
    }

    fn remember(&mut self, k: usize, pal: &Palette) {
        if let Some(s) = pal_slot(k) {
            self.prev_pal[s] = Some(pal.srgb.clone());
        }
    }

    fn finish(
        &mut self,
        out: Encoded,
        exact: bool,
        score: Option<f64>,
        t0: Instant,
    ) -> Encoded {
        self.prev_codec = Some(out.codec);
        self.frame_idx = self.frame_idx.wrapping_add(1);
        self.stats.codec = out.codec;
        self.stats.bytes = out.payload.len();
        self.stats.exact = exact;
        self.stats.score = score;
        self.stats.elapsed = t0.elapsed();
        out
    }

    /// The quality ladder, best first, stopping at the first rung that fits.
    ///
    /// Returns the payload and whether it reproduces the frame exactly. The
    /// bottom of the ladder always fits: `PAL5` at [`MIN_BUDGET`] bytes, and
    /// below that `BC1_DUAL` and then `SOLID`.
    fn ladder(&mut self, f: &Rgb888Frame, budget: usize, clock: &mut Clock) -> (Encoded, bool) {
        let ncol = self.hist.len();
        let chain = self.cfg.profile.chain();

        // Rungs 1-3: the frame's own colours, mathematically lossless. The
        // histogram is already in packed-colour order and `bin_of` is already
        // the index plane, so there is no mapping to do at all.
        if ncol <= 256 {
            for p in 0..NPIX {
                self.idx[p] = self.hist.bin_of()[p] as u8;
            }
            let pal: Vec<[u8; 3]> = self.hist.bins.iter().map(|b| b.srgb).collect();
            if ncol <= 16 && self.allows(codec::PAL4_LZ) {
                clock.mark();
                let v = lz::emit_pal4_lz(&pal, &self.idx, &mut self.lz, chain);
                clock.lap(&mut self.stats.stages.lz);
                if v.len() <= budget {
                    return (
                        Encoded {
                            codec: codec::PAL4_LZ,
                            payload: v,
                        },
                        true,
                    );
                }
            }
            if self.allows(codec::PAL8_LZ) {
                clock.mark();
                let v = lz::emit_pal8_lz(&pal, &self.idx, &mut self.lz, chain);
                clock.lap(&mut self.stats.stages.lz);
                if v.len() <= budget {
                    return (
                        Encoded {
                            codec: codec::PAL8_LZ,
                            payload: v,
                        },
                        true,
                    );
                }
            }
            // An incompressible 32-colour frame can overflow both LZ rungs.
            // A raw PAL5 of the frame's own colours is still exact and is
            // fixed-rate, so it cannot.
            if ncol <= 32 && self.allows(codec::PAL5) && budget >= PAL5_LEN {
                let mut srgb = pal;
                srgb.resize(32, [0, 0, 0]);
                let p = Palette::from_srgb(srgb);
                return (
                    Encoded {
                        codec: codec::PAL5,
                        payload: pal::emit_pal5(&p, &self.idx),
                    },
                    true,
                );
            }
        }

        self.hist.ensure_lab();
        clock.lap(&mut self.stats.stages.lab);

        // Rungs 4-7: adaptive palettes, undithered. Ordered dither adds
        // high-frequency noise that costs LZ far more than it gains.
        for k in [256usize, 128, 64, 32] {
            if k >= ncol || !self.allows(codec::PAL8_LZ) {
                continue;
            }
            clock.mark();
            let seed = self.prev_pal[pal_slot(k).unwrap()].clone();
            let pal = quant::build(&self.hist, k, seed.as_deref(), self.cfg.profile.quant());
            clock.lap(&mut self.stats.stages.quantise);
            let per_bin = quant::nearest_per_bin(&self.hist, &pal);
            for p in 0..NPIX {
                self.idx[p] = per_bin[self.hist.bin_of()[p] as usize];
            }
            clock.lap(&mut self.stats.stages.map);
            let v = lz::emit_pal8_lz(&pal.srgb, &self.idx, &mut self.lz, chain);
            clock.lap(&mut self.stats.stages.lz);
            self.remember(k, &pal);
            if v.len() <= budget {
                return (
                    Encoded {
                        codec: codec::PAL8_LZ,
                        payload: v,
                    },
                    false,
                );
            }
        }

        // Rung 8: 16 colours in a nibble plane.
        if self.allows(codec::PAL4_LZ) {
            clock.mark();
            let seed = self.prev_pal[pal_slot(16).unwrap()].clone();
            let pal = quant::build(&self.hist, 16, seed.as_deref(), self.cfg.profile.quant());
            clock.lap(&mut self.stats.stages.quantise);
            let per_bin = quant::nearest_per_bin(&self.hist, &pal);
            for p in 0..NPIX {
                self.idx[p] = per_bin[self.hist.bin_of()[p] as usize];
            }
            clock.lap(&mut self.stats.stages.map);
            let v = lz::emit_pal4_lz(&pal.srgb, &self.idx, &mut self.lz, chain);
            clock.lap(&mut self.stats.stages.lz);
            self.remember(16, &pal);
            if v.len() <= budget {
                return (
                    Encoded {
                        codec: codec::PAL4_LZ,
                        payload: v,
                    },
                    false,
                );
            }
        }

        // Floor: a fixed-rate payload, so this rung cannot fail.
        if self.allows(codec::PAL5) && budget >= PAL5_LEN {
            clock.mark();
            let seed = self.prev_pal[pal_slot(32).unwrap()].clone();
            let pal = quant::build(&self.hist, 32, seed.as_deref(), self.cfg.profile.quant());
            clock.lap(&mut self.stats.stages.quantise);
            pal::map(
                &self.hist,
                &pal,
                self.cfg.dither,
                self.frame_idx,
                &mut self.idx,
            );
            clock.lap(&mut self.stats.stages.map);
            let payload = pal::emit_pal5(&pal, &self.idx);
            self.remember(32, &pal);
            return (
                Encoded {
                    codec: codec::PAL5,
                    payload,
                },
                false,
            );
        }
        if self.allows(codec::BC1_DUAL) && budget >= BC1_DUAL_LEN {
            clock.mark();
            let payload = block::encode(f, &self.hist);
            clock.lap(&mut self.stats.stages.block);
            return (
                Encoded {
                    codec: codec::BC1_DUAL,
                    payload,
                },
                false,
            );
        }
        // Last resort: the frame's mean colour. Three bytes, always legal.
        let mut acc = [0f64; 3];
        let mut n = 0f64;
        for b in &self.hist.bins {
            for k in 0..3 {
                acc[k] += b.srgb[k] as f64 * b.count as f64;
            }
            n += b.count as f64;
        }
        let c = [
            (acc[0] / n).round() as u8,
            (acc[1] / n).round() as u8,
            (acc[2] / n).round() as u8,
        ];
        (
            Encoded {
                codec: codec::SOLID,
                payload: c.to_vec(),
            },
            false,
        )
    }
}
