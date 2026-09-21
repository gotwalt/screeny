---
id: 248
title: Firmware - a steadier dark end: sub-level bit planes from a narrower output-enable window, and a dither that does not blink at 10 Hz
type: build
hardware: yes (the orchestrator flashes or OTAs; the owner judges by eye; no camera)
depends: []
owner: opus worker (firmware session, 2026-09-21)
branch: card/248-a-steadier-dark-end
---

## Goal

With the device's temporal dither on, the colours closest to black visibly blink at
several hertz when you are within a few feet of the panel (owner, 2026-09-21, a mostly
monochrome clock patch). With the sender snapping to the 64 bit-plane levels instead
(`output.panel: bit_planes`) the blinking stops and the intermediate dark shades go with
it. Give the panel a dark end that has the shades **and** holds still.

## What is going on (read from the code, not yet measured)

- Nothing in the LED drive itself is slower than the 154 Hz refresh. The several-hertz
  component is the dither: `FRAC_BITS = 4` (`firmware/src/gamma.rs`) means a 16-refresh
  cycle, **9.6 Hz**, and `display::render` walks the threshold through the phases in
  counting order (`t = (phase + bayer) & 15`, bump when `frac > t`). So a pixel wanting
  half a level is lit for 8 consecutive refreshes and dark for 8 - a 9.6 Hz square wave -
  and a pixel wanting 1/16 of a level is one flash per 104 ms. The Bayer offset decorrelates
  neighbours; it does nothing for one pixel, and at this pitch the eye resolves one pixel.
  Below level 1 the alternation is black <-> lit, which is the most visible case there is.
  (Card 007 measured "no periodic structure" as a *mean luminance* wobble over the panel;
  that is the spatial average the Bayer offset buys, and not what a person two feet away sees.)
- The root of it is that the smallest unit of light is too big. Level 1 is one
  output-enable window per refresh (9 slots of 64 at the default brightness, 5 at the
  Studio's usual 56) and there is nothing between that and nothing, so the dither makes
  the in-between values by *skipping whole refreshes*.
- The vendored framebuffer can do better. Output-enable is bit 8 of **every entry**, and
  each bit plane has its own copy of the rows
  (`firmware/vendor/hub75-framebuffer/src/bitplane/plain/frame.rs`, `vendor/README.md`),
  so the OE window can differ **per plane**. A plane shown once per refresh (like plane 0)
  with half the window is a real half-level at 154 Hz: a narrower pulse every refresh
  instead of a full pulse every other one. That is BCM extended below the LSB by pulse
  width instead of by repetition count, and it costs one extra plane pass in 63 (~1.6% of
  the refresh rate) and ~4 KB of DMA memory per plane - not the halving of the refresh
  rate that a 7th ordinary plane costs (card 001: 7 planes = 76 Hz, flickers).

## Steps, cheapest first - each one is a build the owner can look at

1. **Bit-reverse the dither phase.** Walk the threshold 0, 8, 4, 12, 2, 10, ... instead of
   0, 1, 2, ...: half a level then alternates every refresh (77 Hz), a quarter is 38 Hz,
   and only the 1/16-weight component is left at 9.6 Hz. Same mean light, by construction -
   keep a host test that every `q` still averages `q/16` over a full cycle, for every Bayer
   offset. Keep the per-pixel offset (check it still decorrelates neighbours after the
   reversal).
2. **Make `FRAC_BITS` a thing that can be compared by eye** at 4, 3 and 2 (a build-time
   constant is enough; a bench-only control op is fine if it is cheap). At 2 the slowest
   component is 38 Hz and the panel still resolves ~250 duty steps. The owner picks.
3. **Sub-level planes.** In the vendored fork, let `bcm_sequence` carry planes below plane
   0 that are shown once per refresh with a narrowed OE window: a half plane, and a quarter
   plane if the window allows. The window is a whole number of slots and depends on the
   runtime brightness, so: define exactly what each sub-plane's width is at each of the 25
   brightness steps, what happens when it rounds to zero (that plane is simply off and its
   weight goes back to the dither), and keep `set_oe_slots` the one place that writes OE.
   The gamma table already carries four fractional bits, so `render` has the information;
   the top fractional bits go to the sub-planes and the rest stay with the (shortened,
   bit-reversed) dither. Brightness must still cost no depth at the top, the cap
   (`MAX_OE_SLOTS`) must not move, and card 136's brightness floor must be respected.
4. **The undithered path rounds instead of truncating.** `q >> FRAC_BITS` sends sRGB 23-33
   to black and puts 22 of the brief's 64 "level" codes on the level below (e.g. 125 -> 12,
   149 -> 18, 156 -> 20). Round to nearest. Separately, consider a dead zone in the dithered
   path: a remainder of 1/16 or 15/16 is a one-refresh blip every 104 ms that carries no
   visible light - snap it to the level.

Stop after any step if the owner says the picture is right; record which steps shipped.

## What is not known until it is on the panel

- **How narrow a pulse the panel's driver chips pass linearly.** One slot is one pixel
  clock; a 2-slot window is a few hundred nanoseconds. The half plane may come out dimmer
  than half, or the quarter plane as nothing. That is what the ramp below is for; if a
  sub-plane is not roughly monotonic, drop it rather than correct for it.
- RAM: ~4 KB of DMA-capable memory per added plane, against research 009/010. Say what the
  headroom is before and after.
- Render cost: `render` is ~3.1 ms of a 6.5 ms refresh with dither on (card 030). More
  planes written per refresh must not push the display task past a refresh.

## Deliverables

- `firmware/src/display.rs`, `firmware/src/gamma.rs`, and the vendored
  `hub75-framebuffer` changes, with `firmware/vendor/README.md` updated to describe them.
- A bench pattern for the owner's eye: a held dark ramp (sRGB 0-70 in a warm monochrome
  and in neutral grey, steps wide enough to tell apart), sendable with the existing sender
  (`crates/screeny` patterns). No camera.
- **The host model follows the device.** `crates/panel` (`DEVICE`, `DITHER_PHASES`, the
  steps count) is checked entry by entry against the firmware; whatever ships here changes
  it, and `docs/design/generative-art-brief.md` section 2.1 and 2.1.1 with it. Card 188
  (the sender's level alignment) reads the same model - leave it a note of the new level
  structure.
- `screeny stats` before and after on a 30 fps stream: refresh rate, fps, drops.

## Acceptance

- The owner, within a few feet, sees no blinking on the dark ramp and on the clock patch
  with the device dither on, and can tell more dark shades apart than with
  `output.panel: bit_planes` today.
- Host tests: mean light per `q` unchanged (or changed only by the step 4 rounding, stated);
  the sub-plane widths are a pure function of brightness with a test at the floor, the
  default, 56 and the cap.
- Refresh rate stays at or above ~145 Hz; the stream holds 30 fps with 0 drops for a
  10-minute run on the final build (one run, per bench discipline).
- No change to the wire protocol. If one turns out to be needed, stop and say so.

## Log

### Stage A - steps 1, 2 and 4 (2026-09-21)

**Where the arithmetic now lives.** `firmware/` is a separate cargo project on the
`esp` toolchain, so nothing in it can be reached from `cargo test`; the only host
check that existed was `crates/panel`'s fixture copy of the gamma *table*, which
says nothing about the order the dither walks its phases in - the very thing this
card is about. So the pure part moved into **`crates/dither` (`screeny-dither`)**:
`no_std`, no float, no I/O, no deps, compiled for xtensa by the firmware and for the
host by `cargo test`, exactly the way `crates/receiver`, `crates/settings` and
`crates/otastate` already are. `firmware/src/gamma.rs` is now the 256-entry sRGB
table and nothing else; `firmware/src/display.rs` calls into the crate.

Every function there takes `frac_bits` as an argument rather than reading the
constant, so one `cargo test` covers all three of step 2's widths whichever width
the build selected. The firmware passes `screeny_dither::FRAC_BITS` and the
compiler folds it.

**Step 1, the bit-reversed phase.** `threshold(phase, bayer, frac_bits)` is
`bit_reverse(phase & mask, frac_bits) ^ (bayer >> (4 - frac_bits))`. Two choices in
that line were not obvious:

- **The reversal is of the phase, not of the sum.** Reversing the phase puts the
  phase counter's bit 0 into the threshold's *top* bit, which is the bit that
  decides a half-level remainder, so that remainder flips every refresh.
- **`XOR`, not addition.** Adding the Bayer offset after the reversal lets a carry
  out of the low bits lengthen a run: with `+` the half-level pattern for bayer 1
  is `1010101010101001` - one run of 2. With `XOR` the sixteen offsets are a
  permutation and the top bit is untouched, so the run is always exactly 1. That
  also makes the spatial average *exactly* constant: at every phase precisely 8 of
  the 16 pixels in a 4x4 block are lit for a half-level remainder (`BAYER4` holds
  each of 0..16 once, so exactly eight have bit 3 set). Card 007's "no periodic
  structure in mean luminance" is now true by construction, not by measurement.

Measured over the whole cycle, at a 154 Hz refresh:

| remainder | before (counting order) | after (bit-reversed) |
|---|---|---|
| 8/16 | 8 refreshes lit, 8 dark - **9.6 Hz** | alternates every refresh - **77 Hz** |
| 4/16 | 4 lit, 12 dark, 9.6 Hz | repeats every 4 refreshes - **38 Hz** |
| 2/16 | 9.6 Hz | repeats every 8 - 19 Hz |
| 1/16 | 9.6 Hz | 9.6 Hz (unchanged; step 4 removes it) |

**Step 4a, rounding in the undithered path.** `quantise_plain` is
`(q + FRAC/2) >> FRAC_BITS`. Truncation lit nothing below `q = 16`, i.e. below sRGB
34; rounding lights from `q = 8`, which is **sRGB 21** (`SRGB_TO_Q[21] == 8`) - the
card said 23, it is 21. The three codes the card named check out: sRGB 125 (`q=207`)
12 -> 13, sRGB 149 (`q=303`) 18 -> 19, sRGB 156 (`q=335`) 20 -> 21.

**Step 4b, the dead zone.** `snap_dead_zone` is defined against the **sixteenths the
table is generated at**, not against `FRAC`: a remainder worth 1/16 of a level or
less is snapped down, 15/16 or more is snapped up onto the next level. At
`frac_bits` 4 that is remainders 1 and 15; at 3 and 2 it is a **no-op**, deliberately
- there the whole cycle is already 19 Hz or 38 Hz and the smallest remainder is
worth an eighth or a quarter of a level, which is real light. Deviation, stated: at
most **1/16 of a level**, on 126 of the 1009 possible `q` values, and in exchange
those 126 stop moving at all.

**Step 2, the three builds.** `frac_bits` is a cargo feature on `screeny-dither`
forwarded by the firmware. The gamma table stays generated at 1008 = 63x16 and is
narrowed at lookup time by `narrow_q`, which **rounds** (`(q + half) >> shift`); at
`frac_bits` 4 the shift is 0 and it compiles to nothing. Three tables would have
been three sets of numbers to check against the sRGB EOTF instead of one.

    cd firmware
    cargo build --release                          # frac_bits 4 (default, ships today)
    cargo build --release --features frac-bits-3   # 8-refresh cycle, 19 Hz, 504 duty steps
    cargo build --release --features frac-bits-2   # 4-refresh cycle, 38 Hz, 252 duty steps

All three built clean on the `esp` toolchain. `FW_VERSION` -> **0.9.0**.

**Render cost.** `bit_reverse` is a loop, so `render` hoists it: `row_thresholds`
runs it four times per row instead of 6,144 times per frame, and what is left in the
pixel loop is one `XOR` where there used to be an add and a mask. The dead zone adds
two comparisons per component. Net effect on `render` should be at or below the
3.1 ms it costs today; not measured, no device.

**Host tests** (14, all three widths, `cargo test -p screeny-dither`):

- `the_mean_over_a_cycle_is_exactly_q` - for every `q` in `0..=63<<frac_bits` and
  every one of the 16 Bayer offsets, the **sum** over one full cycle is exactly `q`,
  so the mean is exactly `q / 2^frac_bits` levels with no rounding error. This is
  the whole claim that the reversal is free.
- `the_thresholds_are_a_permutation_of_the_cycle` - why the above holds.
- `half_a_level_no_longer_blinks` - longest run of identical output for a half-level
  remainder is **1** refresh, for every offset and every width, and asserts in the
  same test that the order that shipped held it for `FRAC/2` refreshes.
- `a_quarter_of_a_level_repeats_every_four_refreshes` - longest run 3, period 4.
- `a_sixteenth_of_a_level_is_still_slow_before_the_dead_zone` - the residue, named.
- `a_four_by_four_block_is_half_lit_at_every_phase` - the spatial claim, and that no
  two of the 16 neighbours share a threshold.
- `the_dead_zone_*` (three tests) - the deviation is <= 1/16 level, on exactly 126
  values, a no-op at widths 3 and 2, and a `q` in the zone produces a run of 16
  (it does not move).
- `the_undithered_path_rounds` - the named codes, the new first-lit code, and that
  no `q` is ever off by more than half a step at any width.
- `narrowing_the_table_rounds_and_keeps_the_endpoints` - monotone, endpoints exact.

**Drive-by:** `cargo clippy --workspace --all-targets` was **not** clean on `main`
(rust-clippy 1.98: `doc_lazy_continuation` in `crates/receiver/tests/identify_overlay.rs`
from card 247, `manual_slice_chunks` in `crates/sim/tests/arbitration.rs`). Both
fixed here, one line each; neither is this card's code.

**Not determined without the panel:** whether 77 Hz is fast enough for this owner at
this pitch, and which of the three `frac_bits` he prefers. That is the point of the
three builds.

### The bench pattern (2026-09-21)

`screeny pattern dark`, in `crates/screeny/src/patterns.rs` beside the other six.
Sixteen four-pixel steps across a 64-column panel, even in **code** (0, 5, 9, 14, 19,
23, 28, 33, 37, 42, 47, 51, 56, 61, 65, 70) rather than even in light, because the
question is "how many of these can you tell apart" and an even code ramp is the one
whose answer is comparable between two builds. Top half warm monochrome
(roughly 2700 K: `[v, 0.80v, 0.55v]`, so sRGB 70 is `[70, 56, 39]`), bottom half
neutral grey at the same `v`. Held: `animated()` is false and `frame(t)` is the same
bytes for every `t`, which matters because the flicker this card is about is on a
*static* picture.

The warm half exists for one reason: its three channels sit on **different**
sub-level remainders at every step, so a dither fault that is per channel shows up
there as a colour shimmer while the neutral half below stays steady. That
distinguishes "the dither is wrong" from "this pixel is wrong".

Four tests in `patterns::tests`: it does not move, the sixteen steps rise to exactly
70 with no two gaps differing by more than one, each step is exactly four pixels wide
and constant down its half, and the top half is warm (`r >= g >= b`) while the bottom
is neutral and ramps with it. `crates/screeny/README.md`'s pattern table gained a row.
