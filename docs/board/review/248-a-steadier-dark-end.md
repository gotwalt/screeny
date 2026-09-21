---
id: 248
title: Firmware - a steadier dark end: sub-level bit planes from a narrower output-enable window, and a dither that does not blink at 10 Hz
type: build
hardware: yes (the orchestrator flashes or OTAs; the owner judges by eye; no camera)
depends: []
owner: opus worker (firmware session, 2026-09-21)
branch: card/248-a-steadier-dark-end
status: review - steps 1, 2 and 4 shipped (fw 0.9.0); step 3 designed and stopped on RAM (card 249)
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

### Stage B - step 3, the sub-level planes: the design, and why it stops here (2026-09-21)

**Result: the mechanism works on paper and does not fit in this build's RAM.** The
design below is complete enough to implement; the number that stops it is
`tools/fw-size.sh`, which reports **`.stack` 25,792 against its own 24,576 floor -
1,216 bytes of headroom** - and one sub-plane costs 4,104. No code was written for
Stage B. Card 249 is the lever.

#### 1. How the DMA scheme accommodates a plane shown once per refresh

`bcm_sequence` streams contiguous plane **suffixes**: segment `i` starts at plane `i`
and covers `i..PLANES` with `reps` chosen so that plane `j`'s total coverage is
`2^j`. Coverage of plane `j` is therefore the sum of the reps of every segment whose
start is `<= j`, and the reps are the first difference of the coverage.

That is exactly what a sub-plane needs, provided the sub-planes sit at the **front**
of memory. Write `SUB` sub-planes at indices `0..SUB` and the six real planes at
`SUB..SUB+6`. The wanted coverage is `1, 1, ..., 1, 1, 2, 4, 8, 16, 32` - flat across
every sub-plane and plane 0 - so the first differences are

    r[0]       = 1
    r[1..=SUB] = 0        <- no segment at all
    r[SUB+1]   = 1
    r[SUB+k]   = 2^(k-1)  for k >= 2

A zero-rep segment is simply omitted, so the sequence is `PLANES - SUB` = **six
segments, the same as today**, and `BCM_SEQUENCE_CAPACITY` (10) is untouched. Only
the first segment spans the sub-planes, and it runs once, so **each sub-plane is
displayed exactly once per refresh** - which is what makes its weight purely a matter
of how wide its output-enable window is. With `SUB = 0` the formula reproduces the
current sequence exactly, so the change is additive.

For `SUB = 2`, `PLANES = 8` (memory order: quarter, half, p0 .. p5):

| segment | starts at | covers | reps |
|---|---|---|---|
| 0 | quarter | 8 planes | 1 |
| 1 | p1 | 5 | 1 |
| 2 | p2 | 4 | 2 |
| 3 | p3 | 3 | 4 |
| 4 | p4 | 2 | 8 |
| 5 | p5 (MSB) | 1 | 16 |

Coverage: quarter 1, half 1, p0 1, p1 2, p2 4, p3 8, p4 16, p5 32. Correct.

The one structural change `frame.rs` needs beyond this is that `bcm_segment_ptr(i)`
currently returns `&planes[i]` - segment index and plane index are the same number,
and with sub-planes they are not. It needs a `const SEG_START: [usize; NSEG]`
alongside `BCM_SEQUENCE`. Everything else (`write_row`, `erase`, `format`,
`set_oe_window`) generalises without thought.

The value written per pixel becomes the level in **quarter-levels**, `n = 4L + s`,
`0..=252` - still a `u8`, so `write_row`'s signature does not change. Plane `k` of
the eight carries bit `k` of `n`: bit 0 is the quarter plane, bit 1 the half plane,
bits 2..8 the six real planes. The bit-index-equals-plane-index identity that
`write_row` relies on survives.

#### 2. The sub-plane window at each of the 25 brightness steps

`set_oe_slots` stays the one place that writes OE, and `MAX_OE_SLOTS` (25) does not
move. Widths are **floor halves of the real window**, `half = lit >> 1` and
`quarter = lit >> 2`, for one reason: floor is the only rounding that guarantees
`quarter <= half` and `half + quarter < lit` at every step, i.e. that the ladder
inside a level never reaches or overtakes the next level. Round-half-up breaks at
`lit = 1` (`half` would be 1, equal to a whole level).

`lit = slots_for(brightness) = (b * 25 + 127) / 255`; card 136's floor is brightness
6, the lowest that lights one slot.

| lit | brightness | half | quarter | light inside one level, in slots |
|---|---|---|---|---|
| 1 | 6..15 (the floor) | 0 | 0 | 0, 0, 0, 0 - **both sub-planes off** |
| 2 | 16..25 | 1 | 0 | 0, 0, 1, 1 - quarter off |
| 3 | 26..35 | 1 | 0 | 0, 0, 1, 1 - quarter off |
| 4 | 36..45 | 2 | 1 | 0, 1, 2, 3 |
| 5 | 46..56 (**the Studio's 56**) | 2 | 1 | 0, 1, 2, 3 |
| 6 | 57..66 | 3 | 1 | 0, 1, 3, 4 |
| 7 | 67..76 | 3 | 1 | 0, 1, 3, 4 |
| 8 | 77..86 | 4 | 2 | 0, 2, 4, 6 |
| 9 | 87..96 (**the default, 96**) | 4 | 2 | 0, 2, 4, 6 |
| 10 | 97..107 | 5 | 2 | 0, 2, 5, 7 |
| 11 | 108..117 | 5 | 2 | 0, 2, 5, 7 |
| 12 | 118..127 | 6 | 3 | 0, 3, 6, 9 |
| 13 | 128..137 | 6 | 3 | 0, 3, 6, 9 |
| 14 | 138..147 | 7 | 3 | 0, 3, 7, 10 |
| 15 | 148..158 | 7 | 3 | 0, 3, 7, 10 |
| 16 | 159..168 (**the cap, 160**) | 8 | 4 | 0, 4, 8, 12 |
| 17 | 169..178 | 8 | 4 | 0, 4, 8, 12 |
| 18 | 179..188 | 9 | 4 | 0, 4, 9, 13 |
| 19 | 189..198 | 9 | 4 | 0, 4, 9, 13 |
| 20 | 199..209 | 10 | 5 | 0, 5, 10, 15 |
| 21 | 210..219 | 10 | 5 | 0, 5, 10, 15 |
| 22 | 220..229 | 11 | 5 | 0, 5, 11, 16 |
| 23 | 230..239 | 11 | 5 | 0, 5, 11, 16 |
| 24 | 240..249 | 12 | 6 | 0, 6, 12, 18 |
| 25 | 250..255 | 12 | 6 | 0, 6, 12, 18 |

**Where a rounded-to-zero sub-plane's weight goes.** A zero-width window is a plane
that emits nothing, so its bit of `n` is light that never arrives. It cannot simply
be left in the value. Two of the 25 steps lose the quarter plane (`lit` 2 and 3) and
one loses both (`lit` 1, the brightness floor). The rule is: **the ladder is built
from the widths, and whatever the ladder cannot reach goes back to the dither.**

That is also the answer to the harder problem the card does not mention: the widths
are integers, so the sub-planes are *not* exactly a half and a quarter. At `lit` 9
the ladder inside a level is 0, 2/9, 4/9, 6/9 where the nominal one is 0, 1/4, 1/2,
3/4 - up to 0.083 of a level out - and at `lit` 5 it is 0, 1/5, 2/5, 3/5, which is
0.15 of a level out and leaves a double-width gap between 3/5 and the next level. If
`render` writes `n = q >> (FRAC_BITS - 2)` and trusts the nominal weights, mean light
per `q` is **not** preserved, and the gap at the top of each level is exactly the
unevenness this card set out to remove.

So the mean-preserving form is: build a 256-entry lookup **when the brightness
changes**, `[u16; 256]`, packing `(n, frac)` where `n` is the ladder point at or
below the target and `frac` is the position between it and the next *distinct*
ladder point, in `FRAC` steps. `render` then costs exactly what it costs today -
one table lookup, one compare, one add - and the mean is exact by construction
because the dither is still choosing between two adjacent ladder points with the
right frequency. Zero-width sub-planes fall out for free: a ladder point that
duplicates the one below it is skipped, and the dither spans the wider gap. The
table is 512 bytes per source table (gamma and linear), rebuilt in the display task
where `BRIGHTNESS_DIRTY` is already handled, in the order of microseconds.

The host tests this wants, none of which were written: the widths are a pure function
of brightness at the floor (lit 1, both off), 56 (lit 5), the default 96 (lit 9) and
the cap 160 (lit 16); the ladder is monotone at every one of the 25 steps; and for
every `q`, the mean light over a cycle equals `q/FRAC` levels exactly, in slot units,
given ideal linear pulses.

#### 3. What it costs

**DMA memory.** `PlaneData<16, 64>` is 16 rows x 64 entries x 2 bytes + a tail and a
padding entry = **2,052 bytes**, and there are two framebuffers, so a sub-plane is
**4,104 bytes** of `.data`. (Cross-check: 6 x 2,052 = 12,312, which is the number
`src/main.rs` already quotes for one buffer.)

Measured on this branch's build with `tools/fw-size.sh`:

| | `.data` | `.stack` | over the 24,576 floor |
|---|---|---|---|
| today (6 planes) | 60,108 | **25,792** | +1,216 |
| one sub-plane | 64,212 | 21,688 | **-2,888** |
| two sub-planes | 68,316 | 17,584 | **-6,992** |

On this chip `.data`, `.bss` and core 0's main stack share one 196,604-byte DRAM
region and the linker fills `.data` and `.bss` first, so `.stack` is the remainder:
every static byte comes straight off it. Research 010 measures core 0's worst
observed depth at **13,328 bytes**, nearly all of it reached at boot, so even two
sub-planes would leave 4,256 bytes of real margin - but the floor in `fw-size.sh` is
24,576 and its own header says a floor under the demand "is worse than no floor,
because it reads as permission". A worker does not get to move it. **This is where
Stage B stops.**

There is no cheaper sub-plane. The DMA stream is one 16-bit entry per pixel clock and
a scan row has to be 64 of them, so a plane that is displayed at all is exactly as
big as any other plane; and the sub-planes have to be *contiguous* with the real ones
for the suffix scheme, so they cannot be moved to the heap on their own. Moving both
whole framebuffers to the heap makes it worse, because the heap arenas are `.bss`
and would have to grow by more than the `.data` freed.

**Refresh rate.** The stream is 1,026 words per plane, `PLANES + 57` plane-streams
per refresh (the 57 is the six real planes' coalesced suffixes, unchanged), at the
10 MHz pixel clock:

| | plane-streams | words | refresh | rate |
|---|---|---|---|---|
| today | 63 | 64,638 | 6.464 ms | **154.7 Hz** |
| + 1 sub-plane | 64 | 65,664 | 6.566 ms | 152.3 Hz |
| + 2 sub-planes | 65 | 66,690 | 6.669 ms | 149.9 Hz |

Both clear the card's ~145 Hz. This is the cheap part, and it is the whole reason a
sub-plane beats a seventh ordinary plane (card 001: 7 planes = 76 Hz).

**`render`.** Card 030 measures ~3.1 ms of the 6.5 ms refresh, and `write_row`'s
inner loop is `PLANES x COLS` per row, so it scales with the plane count: ~3.6 ms
with one sub-plane, ~4.1 ms with two, against a refresh that has grown to 6.57 /
6.67 ms. The margin falls from 3.4 ms to 2.6 ms but the display task still finishes
inside a refresh, which is the condition that matters (it free-runs; `swap` blocks
to the frame boundary). `set_oe_slots` grows from 6,144 to 8,192 entries but runs
only when the brightness changes.

#### 4. What could not be determined without the panel

- **How narrow a pulse this panel's driver passes linearly.** At `lit` 9 the quarter
  plane is 2 pixel clocks, 200 ns. It may come out at less than a quarter, or at
  nothing. The card's own instruction stands: if a sub-plane is not roughly
  monotonic on the ramp, drop it rather than correct for it.
- Whether 149.9 Hz is visibly worse than 154.7 Hz on this panel.
- The real `render` cost at 8 planes; the figures above are scaled from card 030.

### What the host model has to change to, once the owner has chosen (2026-09-21)

**Not done here, on purpose:** card 188 is in `crates/panel` right now, and
`docs/design/generative-art-brief.md` 2.1/2.1.1 follows whatever `crates/panel` says.
This is the note the card asked for instead. The numbers below are computed from the
same sRGB EOTF `SRGB_TO_Q` is generated from, so they can be checked without a device.

**Nothing changes on the wire.** No opcode, no field, no frame format. The only thing
that moves is which duty a code lands on.

#### 1. If the owner keeps `frac_bits` 4 (the default build)

- `DITHER_PHASES` stays **16** and `DEVICE.steps` stays **1008**. The bit-reversal is
  a reordering of the phases; the mean over a cycle is unchanged for every `q`, and
  `crates/dither`'s `the_mean_over_a_cycle_is_exactly_q` is the proof. `DEVICE` as a
  *mean* model needs no change for step 1.
- The **dead zone** does change `DEVICE`. `emit1(v)` is no longer `SRGB_TO_Q[v]/1008`
  but `snap_dead_zone(SRGB_TO_Q[v], 4)/1008`: a remainder of 1/16 snaps down, 15/16
  snaps up. It moves **34 of the 256 codes**, and:

  | | `DEVICE` today | `DEVICE` with the dead zone |
  |---|---|---|
  | distinct levels from 256 codes | 237 | **229** |
  | codes that come out black | 2 | **5** |
  | darkest lit code | sRGB 2 | **sRGB 5** |

  That is the honest cost of step 4b: eight distinct shades and three codes, all of
  them things that were emitting one sixteenth of a level as a 9.6 Hz blip. The test
  `device_is_the_firmwares_gamma_table` and the table in `DEVICE`'s doc comment both
  need these numbers; `the_dark_end_is_what_the_brief_measured` needs `first_lit` 5.

#### 2. If the owner picks a shorter cycle

| | `frac_bits` 4 | 3 | 2 |
|---|---|---|---|
| `DITHER_PHASES` | 16 | **8** | **4** |
| `DEVICE.steps` (= `63 << frac_bits`) | 1008 | **504** | **252** |
| distinct levels from 256 codes | 229 (with the dead zone) | **217** | **182** |
| codes that come out black | 5 | 2 | 5 |
| darkest lit code | sRGB 5 | sRGB 2 | sRGB 5 |
| slowest visible component | 9.6 Hz -> removed | 19 Hz | 38 Hz |

At 3 and 2 the dead zone is a **no-op** (it is defined against the sixteenths the
table is generated at), so `emit1(v)` there is `narrow_q(SRGB_TO_Q[v], frac_bits) /
(63 << frac_bits)` - narrowing by rounding, not truncation. `DEVICE` stays
`Panel::dithered(6, DITHER_PHASES)`; only the constant moves.

`TEMPORAL` (`Panel::dithered(6, 5)`, the encoder's within-one-frame model) is "about
five of the sixteen phases". At `frac_bits` 3 a 30 fps frame gets about five of
**eight**, which is most of the cycle, and at 2 it gets more than a full cycle - so at
either of those `TEMPORAL` should simply become `DEVICE`. That is a simplification,
not a loss.

#### 3. The one real disagreement this card found

**`crates/panel`'s `NOMINAL` has always rounded, and the firmware truncated.**
`Panel::emit1` is `(SRGB_TO_LIN[v] * steps).round() / steps`, so `NOMINAL`
(`Panel::new(6)`, 63 steps) says the darkest lit code is sRGB 22. The firmware's
undithered path was `q >> 4`, which made it sRGB **34** and disagreed with the model
at **109 of the 256 codes**. That path is what `output.panel: bit_planes` shows, which
is the setting the owner switched to when the dither blinked - so the sender has been
snapping to a ladder the device did not have.

Card 248 step 4a makes the firmware round, which cuts the disagreement from 109 codes
to **7** (sRGB 21, 56, 123, 151, 182, 212, 223). Those seven are double-rounding: the
table is `round(1008 * lin)` and the firmware then rounds *that* to a level, while
`NOMINAL` rounds `lin` to a level in one step. sRGB 21 is the visible one - the
firmware lights it, `NOMINAL` does not.

The fix is not to chase the rounding but to define the host model from the firmware's
table, the way `DEVICE` already is: `NOMINAL.emit1(v)` should be
`quantise_plain(SRGB_TO_Q[v], 4) / 63`. Then the two agree at all 256 codes by
construction, and the fixture test that already lives in `crates/panel` covers both
halves instead of one.

#### 4. What card 188 needs to know

- The **held** ladder (`output.panel` dithered) is unchanged in shape - 63 levels x
  `DITHER_PHASES` phases - and changed only by the dead zone at the bottom: the three
  extra black codes and the eight fewer distinct shades above.
- The **bit-planes** ladder moves by half a level everywhere: 116 of the 256 codes now
  land one level higher than they did, and the first lit code moves from 34 to 21.
  A sender that snaps to bit-plane levels has to re-derive them.
- Nothing about brightness changes: dimming is still the output-enable window,
  `oe_slots` is still `(b * 25 + 127) / 255`, the floor is still 6 and the cap 160.
- Had stage B shipped, the held ladder would have become **brightness-dependent** -
  the sub-level steps inside a level are `quarter/lit` and `half/lit` with integer
  widths, so `DEVICE` would have had to take a brightness. It did not ship, so it does
  not. Card 249 carries that warning forward.

### Handover (2026-09-21)

**What shipped on this branch: steps 1, 2 and 4, plus the bench pattern. Step 3 is a
design and a stop.** Firmware **0.9.0**.

Three builds for the owner to compare, from `firmware/`:

    cargo build --release                          # frac_bits 4 - 16-refresh cycle
    cargo build --release --features frac-bits-3   # 8-refresh cycle
    cargo build --release --features frac-bits-2   # 4-refresh cycle

and the pattern to look at them with, with the device dither **on**:

    screeny pattern dark --addr 192.168.7.221 --duration 60

Every one of the three already has step 1 (bit-reversed phases), step 4a (rounding
with the dither off) and, at `frac_bits` 4 only, step 4b (the dead zone). So the
question the three builds ask is narrow: *is 77 Hz enough, or does the whole cycle
need to be shorter?* If `frac_bits` 4 is steady, ship it - it is the one with the
most shades (229 against 217 and 182).

**What to look for on the `dark` pattern**, in order: does any step blink, breathe or
shimmer from two feet; how many of the sixteen steps can be told apart (compare
against `output.panel: bit_planes`, where it should be far fewer); and does the warm
half shimmer *in colour* while the grey half below is steady - that would be a
per-channel fault rather than a per-pixel one, and would want its own card.

`screeny stats` before and after on a 30 fps stream is the orchestrator's; nothing in
stage A changes the frame path, the swap rate or the DMA stream, so refresh rate, fps
and drops should be identical to 0.8.2. `tools/fw-size.sh` on this build: `.data`
60,108, `.bss` 110,704, `.stack` **25,792** (floor 24,576) - unchanged, the arithmetic
move added no statics.

**Not done, deliberately:**

- `crates/panel` and `docs/design/generative-art-brief.md` 2.1/2.1.1 - card 188 is in
  that crate. The section above says exactly what has to change to, and it depends on
  which of the three builds the owner picks.
- Stage B itself. Card **249** is in `backlog/`: find 4-8 KB of DRAM.
- `firmware/`'s own `cargo clippy` has seven pre-existing warnings (`build.rs`,
  `http.rs`, `ota.rs`, `receiver.rs`, `screens.rs`, `store.rs`); none is in the code
  this card touched, and the project's clippy rule is about the host workspace, which
  **is** clean.

**Nothing in the wire protocol moved**, and nothing needed to.
