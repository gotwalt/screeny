//! The panel's sub-level arithmetic, with no I/O and no device in it.
//!
//! The panel has [`LEVELS`] = 63 duty levels above black ([`PLANES`] = 6 bit
//! planes). `firmware/src/gamma.rs` keeps the sRGB EOTF at sixteen times that
//! resolution — entry `q` means "this code wants `q`/16 of a level", top entry
//! [`Q_SCALE`] = 1008 — because quantising the EOTF to 64 levels sends the
//! bottom 34 sRGB codes to black. Everything that turns such a `q` into the
//! level to light *on this refresh* is in this file, and in this file only:
//! the firmware calls it from `display::render` and `cargo test` calls it from
//! the tests at the bottom.
//!
//! That split is card 248. Before it the arithmetic lived in
//! `firmware/src/display.rs`, which is a separate cargo project on the `esp`
//! toolchain and therefore unreachable from a host test — the only check that
//! existed was `crates/panel`'s fixture copy of the *table*, which says
//! nothing about the *order* the dither walks its phases in. The order is what
//! card 248 is about.
//!
//! # Why the order matters
//!
//! The dither spends the remainder `q & (FRAC-1)` across [`FRAC`] successive
//! panel refreshes by lighting the level above on some of them. With the
//! threshold walked in counting order — `t = (phase + bayer) & 15`, which is
//! what shipped up to firmware 0.8.2 — a pixel wanting half a level is lit for
//! **eight consecutive refreshes and dark for eight**. At a 154 Hz refresh
//! that is a 9.6 Hz square wave on every pixel whose colour happens to land
//! there, and at this pixel pitch a person a few feet away sees it blink
//! (owner, 2026-09-21).
//!
//! The mean over a full cycle does not care what order the thresholds come in,
//! only that each of the [`FRAC`] values is visited once. So [`threshold`]
//! visits them **bit-reversed**: 0, 8, 4, 12, 2, 10, … The same amount of
//! light, arranged so that the big components move fast:
//!
//! | remainder | lit refreshes per cycle | repeats every | at 154 Hz |
//! |-----------|------------------------|---------------|-----------|
//! | 8/16 (half a level)    | 8 | 2 refreshes  | 77 Hz   |
//! | 4/16 (a quarter)       | 4 | 4 refreshes  | 38 Hz   |
//! | 2/16                   | 2 | 8 refreshes  | 19 Hz   |
//! | 1/16                   | 1 | 16 refreshes | 9.6 Hz  |
//!
//! Only the last row is left slow, and it carries a sixteenth of a level —
//! one brief flash per 104 ms that adds no visible light. [`snap_dead_zone`]
//! removes it.
//!
//! # `frac_bits` is an argument, not a constant
//!
//! Every function here takes the number of fractional bits rather than reading
//! [`FRAC_BITS`], for two reasons. The firmware passes the constant and the
//! compiler folds the whole thing away; the tests run all three widths in one
//! `cargo test`, whichever width the build happens to select. Card 248 step 2
//! wants three builds the owner can compare by eye, and a constant that only
//! one of them exercises is a constant whose other values are untested.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

// ---------------------------------------------------------------------------
// The panel's shape
// ---------------------------------------------------------------------------

/// Bit planes per channel, and therefore duty levels `0..=`[`LEVELS`].
pub const PLANES: usize = 6;

/// The brightest duty level: 63.
pub const LEVELS: u16 = (1 << PLANES) - 1;

/// Fractional bits `firmware/src/gamma.rs`'s table is generated at, and so
/// the widest [`FRAC_BITS`] that costs nothing: `SRGB_TO_Q[255]` is
/// [`Q_SCALE`] = `LEVELS << TABLE_FRAC_BITS`.
pub const TABLE_FRAC_BITS: u16 = 4;

/// Top entry of the sRGB table: `63 * 16`.
pub const Q_SCALE: u16 = LEVELS << TABLE_FRAC_BITS;

// ---------------------------------------------------------------------------
// How many fractional bits this build spends
// ---------------------------------------------------------------------------

#[cfg(all(feature = "frac-bits-3", feature = "frac-bits-2"))]
compile_error!("pick one of frac-bits-3 / frac-bits-2, or neither for the default 4");

/// Fractional bits carried below one duty level, and therefore the length of
/// the dither cycle in refreshes ([`FRAC`]).
///
/// Four is the default and is what shipped before card 248: sixteen refreshes,
/// 9.6 Hz, and 1008 duty steps per channel. The cargo features `frac-bits-3`
/// and `frac-bits-2` shorten the cycle to eight (19 Hz, 504 steps) and four
/// (38 Hz, 252 steps); the panel still resolves far more than the 64 levels
/// the bit planes alone give, and the slowest thing on it moves faster than
/// the eye follows. Which of the three ships is the owner's choice, made by
/// looking at three builds.
pub const FRAC_BITS: u16 = if cfg!(feature = "frac-bits-2") {
    2
} else if cfg!(feature = "frac-bits-3") {
    3
} else {
    4
};

/// Dither phases per cycle: `1 << FRAC_BITS`.
pub const FRAC: u16 = 1 << FRAC_BITS;

// ---------------------------------------------------------------------------
// The per-pixel phase offset
// ---------------------------------------------------------------------------

/// 4x4 ordered (Bayer) matrix, as a per-pixel *phase offset* rather than as a
/// spatial dither.
///
/// Without it every pixel with the same remainder would toggle on the same
/// refresh and the whole panel would beat together. With it — and with
/// [`threshold`] combining the two by `XOR` rather than by addition — the
/// offsets are a permutation of `0..16`, so at any phase **exactly eight of
/// the sixteen pixels in a 4x4 block are lit** for a remainder of half a
/// level: the spatial mean is not merely close to constant, it is constant.
/// Card 007's "no periodic structure in mean luminance" is now true by
/// construction instead of by measurement.
pub const BAYER4: [[u16; 4]; 4] = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
];

/// The Bayer offset for a pixel, from [`BAYER4`].
#[inline(always)]
#[must_use]
pub const fn bayer(x: usize, y: usize) -> u16 {
    BAYER4[y & 3][x & 3]
}

// ---------------------------------------------------------------------------
// The arithmetic
// ---------------------------------------------------------------------------

/// Reverse the low `bits` bits of `x`; the rest are dropped.
#[inline]
#[must_use]
pub const fn bit_reverse(x: u16, bits: u16) -> u16 {
    let mut out = 0u16;
    let mut i = 0;
    while i < bits {
        out = (out << 1) | ((x >> i) & 1);
        i += 1;
    }
    out
}

/// The dither threshold for refresh `phase` at a pixel whose [`BAYER4`]
/// offset is `bayer`.
///
/// `bayer` is always a full 4-bit offset; it is narrowed here, so the caller
/// has one table whatever `frac_bits` is.
///
/// The reversal is of the *phase*, so the threshold's most significant bit —
/// the one that decides a half-level remainder — is the phase counter's least
/// significant bit and flips every refresh. `XOR` with the offset then moves a
/// pixel within the cycle without changing how fast any bit of it moves, which
/// plain addition would (a carry out of the low bits lengthens a run).
///
/// The firmware calls this once per frame per Bayer column, not once per
/// pixel: `bit_reverse` is a loop, and `phase` does not vary within a frame.
#[inline]
#[must_use]
pub const fn threshold(phase: u16, bayer: u16, frac_bits: u16) -> u16 {
    debug_assert!(frac_bits >= 1 && frac_bits <= TABLE_FRAC_BITS);
    let mask = (1u16 << frac_bits) - 1;
    bit_reverse(phase & mask, frac_bits) ^ (bayer >> (TABLE_FRAC_BITS - frac_bits))
}

/// Narrow a table entry (in sixteenths of a level, `0..=`[`Q_SCALE`]) to
/// `frac_bits` fractional bits, **rounding rather than truncating**.
///
/// `frac_bits` = 4 is the identity. The table stays generated at one scale so
/// that only one set of numbers is ever checked against the sRGB EOTF; the
/// three comparison builds differ by this shift and nothing else.
#[inline(always)]
#[must_use]
pub const fn narrow_q(q: u16, frac_bits: u16) -> u16 {
    let shift = TABLE_FRAC_BITS - frac_bits;
    let half = (1u16 << shift) >> 1;
    (q + half) >> shift
}

/// Snap a remainder worth a sixteenth of a level or less onto the level.
///
/// A remainder of 1/16 is one lit refresh in sixteen and a remainder of 15/16
/// is one dark refresh in sixteen. Both are a 9.6 Hz blip carrying a
/// sixteenth of a level — below the panel's own step and well below anything
/// the eye reads as brightness, but not below what it reads as *movement*.
/// Snapping them costs at most 1/16 of a level of accuracy and removes the
/// only component [`threshold`] leaves slow.
///
/// Defined against the sixteenths the table is generated at, so it is a no-op
/// at `frac_bits` 3 and 2: there the whole cycle is already 19 Hz or 38 Hz and
/// the smallest remainder is worth an eighth or a quarter of a level, which is
/// real light and must not be thrown away.
#[inline(always)]
#[must_use]
pub const fn snap_dead_zone(q: u16, frac_bits: u16) -> u16 {
    let frac = 1u16 << frac_bits;
    let dead = frac >> TABLE_FRAC_BITS; // 1 at frac_bits 4, else 0
    let rem = q & (frac - 1);
    if rem <= dead {
        q - rem
    } else if rem + dead >= frac {
        q + (frac - rem)
    } else {
        q
    }
}

/// The level to light on this refresh: `q >> frac_bits`, plus one when the
/// remainder beats `threshold`.
///
/// **No dead zone.** This is the mean-preserving core: over one full cycle of
/// [`FRAC`] phases the thresholds are a permutation of `0..FRAC`, so exactly
/// `q & (FRAC-1)` of them are beaten and the mean is exactly `q / FRAC`
/// levels, for every Bayer offset. The tests say so for every `q`.
#[inline(always)]
#[must_use]
pub const fn quantise_raw(q: u16, threshold: u16, frac_bits: u16) -> u8 {
    let level = q >> frac_bits;
    let frac = q & ((1u16 << frac_bits) - 1);
    let bumped = level + (frac > threshold) as u16;
    if bumped > LEVELS {
        LEVELS as u8
    } else {
        bumped as u8
    }
}

/// [`snap_dead_zone`] then [`quantise_raw`]: what `display::render` calls.
#[inline(always)]
#[must_use]
pub const fn quantise_dither(q: u16, threshold: u16, frac_bits: u16) -> u8 {
    quantise_raw(snap_dead_zone(q, frac_bits), threshold, frac_bits)
}

/// The level to light with the dither off: `q / (1 << frac_bits)` **rounded to
/// nearest**.
///
/// Truncating, which is what shipped before card 248, sends sRGB 23..=33 to
/// black and lands 22 of the 64 level codes on the level below the one they
/// were chosen for (sRGB 125 -> 12 not 13, 149 -> 18 not 19, 156 -> 20 not
/// 21). Rounding costs nothing and halves the worst-case error.
#[inline(always)]
#[must_use]
pub const fn quantise_plain(q: u16, frac_bits: u16) -> u8 {
    let half = 1u16 << (frac_bits - 1);
    let n = (q + half) >> frac_bits;
    if n > LEVELS {
        LEVELS as u8
    } else {
        n as u8
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    /// Every `frac_bits` a build can select, so `cargo test` covers all three
    /// whatever the features say.
    const WIDTHS: [u16; 3] = [4, 3, 2];

    /// The top `q` at a given width: `LEVELS << frac_bits`.
    const fn top_q(frac_bits: u16) -> u16 {
        LEVELS << frac_bits
    }

    /// The threshold order that shipped up to firmware 0.8.2, kept here and
    /// nowhere else so the tests can say what changed.
    fn counting_threshold(phase: u16, bayer: u16, frac_bits: u16) -> u16 {
        (phase + (bayer >> (TABLE_FRAC_BITS - frac_bits))) & ((1 << frac_bits) - 1)
    }

    /// One dither cycle of outputs for a held `q`.
    fn cycle(q: u16, bayer: u16, frac_bits: u16) -> Vec<u8> {
        (0..(1u16 << frac_bits))
            .map(|phase| quantise_raw(q, threshold(phase, bayer, frac_bits), frac_bits))
            .collect()
    }

    fn longest_run(v: &[u8]) -> usize {
        // The cycle repeats, so a run may wrap; walk it twice and cap at the
        // cycle length.
        let n = v.len();
        let mut best = 1;
        let mut run = 1;
        for i in 1..2 * n {
            if v[i % n] == v[(i - 1) % n] {
                run += 1;
                best = best.max(run.min(n));
            } else {
                run = 1;
            }
        }
        best
    }

    // -----------------------------------------------------------------
    // The one thing the dither must not change: how much light comes out
    // -----------------------------------------------------------------

    /// **Every `q`, every Bayer offset, exact.** The sum over one cycle is
    /// `q`, so the mean is `q / 2^frac_bits` levels with no rounding error at
    /// all. This is what makes the bit-reversal free: it permutes the
    /// thresholds, and a permutation of `0..FRAC` beats a remainder exactly as
    /// often however it is ordered.
    #[test]
    fn the_mean_over_a_cycle_is_exactly_q() {
        for frac_bits in WIDTHS {
            for q in 0..=top_q(frac_bits) {
                for b in 0..16u16 {
                    let sum: u32 = cycle(q, b, frac_bits).iter().map(|&l| u32::from(l)).sum();
                    assert_eq!(
                        sum,
                        u32::from(q),
                        "frac_bits {frac_bits}, q {q}, bayer {b}: sum over the cycle"
                    );
                }
            }
        }
    }

    /// The same for the whole sRGB table at the default width, which is the
    /// case that actually reaches the panel: 0..=1008 is a superset, but this
    /// is the sentence `crates/panel`'s `DEVICE` model makes.
    #[test]
    fn the_thresholds_are_a_permutation_of_the_cycle() {
        for frac_bits in WIDTHS {
            for b in 0..16u16 {
                let mut seen: Vec<u16> = (0..(1u16 << frac_bits))
                    .map(|p| threshold(p, b, frac_bits))
                    .collect();
                seen.sort_unstable();
                let want: Vec<u16> = (0..(1u16 << frac_bits)).collect();
                assert_eq!(seen, want, "frac_bits {frac_bits}, bayer {b}");
            }
        }
    }

    // -----------------------------------------------------------------
    // Card 248 step 1: what the eye sees
    // -----------------------------------------------------------------

    /// **Half a level alternates every single refresh.** The number this card
    /// exists for: it was eight refreshes on, eight off (9.6 Hz at a 154 Hz
    /// refresh); it is now one on, one off (77 Hz). Asserted against the old
    /// order in the same test so the change cannot be read as an accident.
    #[test]
    fn half_a_level_no_longer_blinks() {
        for frac_bits in WIDTHS {
            let half = 1u16 << (frac_bits - 1);
            for level in [0u16, 1, 7, 62] {
                let q = (level << frac_bits) | half;
                for b in 0..16u16 {
                    assert_eq!(
                        longest_run(&cycle(q, b, frac_bits)),
                        1,
                        "frac_bits {frac_bits}, level {level}, bayer {b}"
                    );

                    let old: Vec<u8> = (0..(1u16 << frac_bits))
                        .map(|p| {
                            quantise_raw(q, counting_threshold(p, b, frac_bits), frac_bits)
                        })
                        .collect();
                    assert_eq!(
                        longest_run(&old),
                        usize::from(half),
                        "the order that shipped held half a level for {half} refreshes"
                    );
                }
            }
        }
    }

    /// A quarter of a level repeats every four refreshes (38 Hz), not every
    /// sixteen. Three dark then one lit is the longest run it can produce.
    #[test]
    fn a_quarter_of_a_level_repeats_every_four_refreshes() {
        for frac_bits in [4u16, 3] {
            let quarter = 1u16 << (frac_bits - 2);
            for b in 0..16u16 {
                let c = cycle(quarter, b, frac_bits);
                assert_eq!(longest_run(&c), 3, "frac_bits {frac_bits}, bayer {b}");
                // Period four: the pattern repeats on every fourth phase.
                for p in 0..c.len() {
                    assert_eq!(c[p], c[(p + 4) % c.len()], "frac_bits {frac_bits}, phase {p}");
                }
            }
        }
    }

    /// The residue card 248 step 4 removes: a sixteenth of a level is one lit
    /// refresh in sixteen however the thresholds are ordered, which is still
    /// 9.6 Hz. Reversal cannot help here — there is only one lit phase.
    #[test]
    fn a_sixteenth_of_a_level_is_still_slow_before_the_dead_zone() {
        let c = cycle(1, 0, 4);
        assert_eq!(c.iter().filter(|&&l| l == 1).count(), 1);
        assert_eq!(longest_run(&c), 15);
    }

    /// The Bayer offset still decorrelates neighbours after the reversal, and
    /// does it better: with `XOR` the sixteen offsets are a permutation, so at
    /// **every** phase exactly eight of a 4x4 block are lit for a half-level
    /// remainder. The panel-wide mean therefore has no temporal ripple at all,
    /// and no two neighbours share a threshold.
    #[test]
    fn a_four_by_four_block_is_half_lit_at_every_phase() {
        for frac_bits in WIDTHS {
            let q = 1u16 << (frac_bits - 1);
            for phase in 0..(1u16 << frac_bits) {
                let mut lit = 0;
                let mut seen: Vec<u16> = Vec::new();
                for y in 0..4 {
                    for x in 0..4 {
                        let t = threshold(phase, bayer(x, y), frac_bits);
                        seen.push(t);
                        lit += u32::from(quantise_raw(q, t, frac_bits));
                    }
                }
                assert_eq!(lit, 8, "frac_bits {frac_bits}, phase {phase}");
                // Each threshold appears 16 / FRAC times: never twice among
                // neighbours at the full width.
                seen.sort_unstable();
                seen.dedup();
                assert_eq!(seen.len(), usize::from(1u16 << frac_bits));
            }
        }
    }

    // -----------------------------------------------------------------
    // Card 248 step 4
    // -----------------------------------------------------------------

    /// The dead zone's whole deviation, stated rather than hidden: it moves
    /// `q` by at most one sixteenth of a level, only at the two extreme
    /// remainders, and only at `frac_bits` 4.
    #[test]
    fn the_dead_zone_moves_q_by_at_most_a_sixteenth_of_a_level() {
        for q in 0..=top_q(4) {
            let snapped = snap_dead_zone(q, 4);
            let rem = q & 15;
            let want = match rem {
                0 | 1 => q - rem,
                15 => q + 1,
                _ => q,
            };
            assert_eq!(snapped, want, "q {q}");
            assert!(snapped.abs_diff(q) <= 1);
            assert!(snapped <= top_q(4), "q {q} cannot snap past the top level");
            if snapped != q {
                assert_eq!(snapped & 15, 0, "the zone always lands on a level");
            }
        }
        // Exactly 2 of every 16 remainders are in the zone.
        let moved = (0..=top_q(4)).filter(|&q| snap_dead_zone(q, 4) != q).count();
        assert_eq!(moved, 126);
    }

    #[test]
    fn the_dead_zone_is_a_no_op_at_the_shorter_cycles() {
        for frac_bits in [3u16, 2] {
            for q in 0..=top_q(frac_bits) {
                assert_eq!(snap_dead_zone(q, frac_bits), q, "frac_bits {frac_bits}, q {q}");
            }
        }
    }

    /// With the dead zone in, the mean is still within a sixteenth of a level
    /// of `q`, and a held colour in the zone holds *still*.
    #[test]
    fn the_dithered_path_is_steady_inside_the_dead_zone() {
        for q in 0..=top_q(4) {
            for b in 0..16u16 {
                let c: Vec<u8> = (0..16)
                    .map(|p| quantise_dither(q, threshold(p, b, 4), 4))
                    .collect();
                let sum: u32 = c.iter().map(|&l| u32::from(l)).sum();
                assert!(
                    sum.abs_diff(u32::from(q)) <= 1,
                    "q {q} bayer {b}: cycle sums to {sum}"
                );
                if matches!(q & 15, 0 | 1 | 15) {
                    assert_eq!(longest_run(&c), 16, "q {q} bayer {b} should not move at all");
                }
            }
        }
    }

    /// Rounding instead of truncating, and exactly which codes it moves.
    #[test]
    fn the_undithered_path_rounds() {
        // The three codes the card named, as their table entries:
        // sRGB 125 -> 207, 149 -> 303, 156 -> 335.
        for (q, want) in [(207u16, 13u8), (303, 19), (335, 21)] {
            assert_eq!(quantise_plain(q, 4), want, "q {q}");
            assert_eq!(q >> 4, u16::from(want) - 1, "q {q}: truncation lost a level");
        }
        // Truncation lit nothing below q = 16, i.e. below sRGB 34. Rounding
        // lights from q = 8, i.e. from sRGB 21 (`SRGB_TO_Q[21] == 8`).
        assert_eq!(quantise_plain(7, 4), 0);
        assert_eq!(quantise_plain(8, 4), 1);
        assert_eq!(15u16 >> 4, 0, "truncation was black all the way to 16");
        // 22 of the 64 level codes landed a level low under truncation: the
        // remainders 8..=15, which are 22 of the table's 256 entries.
        let low = (0..=Q_SCALE)
            .filter(|q| u16::from(quantise_plain(*q, 4)) != q >> 4)
            .count();
        assert_eq!(low, 63 * 8, "half of every level's remainders round up");
        // Never above the top level, at any width.
        for frac_bits in WIDTHS {
            assert_eq!(quantise_plain(top_q(frac_bits), frac_bits), LEVELS as u8);
            for q in 0..=top_q(frac_bits) {
                let got = u16::from(quantise_plain(q, frac_bits));
                assert!(got <= LEVELS);
                let err = (got << frac_bits).abs_diff(q);
                assert!(err <= 1 << (frac_bits - 1), "q {q}: off by {err}");
            }
        }
    }

    // -----------------------------------------------------------------
    // Card 248 step 2
    // -----------------------------------------------------------------

    /// Narrowing the table is rounding, not truncation, and the endpoints are
    /// exact: the top code is still the top level at every width.
    #[test]
    fn narrowing_the_table_rounds_and_keeps_the_endpoints() {
        for frac_bits in WIDTHS {
            assert_eq!(narrow_q(0, frac_bits), 0);
            assert_eq!(narrow_q(Q_SCALE, frac_bits), top_q(frac_bits));
            let mut last = 0;
            for q in 0..=Q_SCALE {
                let n = narrow_q(q, frac_bits);
                assert!(n >= last, "q {q}: narrowing must not go backwards");
                last = n;
                let shift = TABLE_FRAC_BITS - frac_bits;
                // Within half a step of the exact value, which truncation is
                // not: truncation is within a whole step, one-sided.
                assert!((n << shift).abs_diff(q) <= (1 << shift) / 2, "q {q}");
            }
        }
    }

    /// What this build selected, and that the feature set is coherent.
    #[test]
    fn the_selected_width_is_one_of_the_three() {
        assert!(WIDTHS.contains(&FRAC_BITS));
        assert_eq!(FRAC, 1 << FRAC_BITS);
        assert_eq!(
            FRAC_BITS,
            if cfg!(feature = "frac-bits-2") {
                2
            } else if cfg!(feature = "frac-bits-3") {
                3
            } else {
                4
            }
        );
    }

    #[test]
    fn bit_reverse_is_its_own_inverse() {
        for bits in 1..=4u16 {
            for x in 0..(1u16 << bits) {
                assert_eq!(bit_reverse(bit_reverse(x, bits), bits), x);
            }
        }
        assert_eq!(bit_reverse(1, 4), 8);
        assert_eq!(bit_reverse(2, 4), 4);
        assert_eq!(bit_reverse(3, 4), 12);
    }

    #[test]
    fn the_bayer_matrix_is_a_permutation() {
        let mut seen: Vec<u16> = (0..4).flat_map(|y| (0..4).map(move |x| bayer(x, y))).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..16u16).collect::<Vec<_>>());
    }
}
