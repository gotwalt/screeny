---
id: 188
title: Art - held dark shades sit on a panel level: the aligned-code table in the panel model, a dark-end snap that leaves the brights alone, and the clocks patch on it
type: build
hardware: no (the owner looks at the result on the panel through the Studio)
depends: []
owner: worker (sonnet)
branch: card/188-held-dark-shades
---

## Goal

The owner's clocks patch (2026-09-21) has two bad choices today. With
`output.panel: dithered` its dim shades - the ghost dots, the soft edges of the hands -
blink at several hertz up close, because they sit between hardware levels and the device
makes them by alternating "off" and "on" at 9.6 Hz. With `output.panel: bit_planes` they
hold still, and some of them disappear, because the whole frame is flattened to 64 levels.
Make the third choice: **keep the device's resolution where it is invisible, and put what
is held and dark exactly on a level.** `docs/design/generative-art-brief.md` section 2.1.1
is the guidance this card implements; read it first.

## Context

- A code is steady when its duty in the firmware's gamma table (`firmware/src/gamma.rs`,
  `SRGB_TO_Q`, sixteenths of a level) is a whole level. For levels 0-16 the lowest code
  that reaches the level is within 2/16 of it and is right whether the device dithers or
  not: `0 34 50 62 71 80 87 94 100 106 111 116 121 126 130 134 138`.
- `screeny_panel::NOMINAL.snap` / `screeny_art::panel::Panel::BitPlanes.snap8` answer a
  different question - the *nearest* code to a level's light - and 22 of those 64 codes sit
  a hair under their level (125, 149, 156, ...). Harmless with the device dither on; one
  level low with it off. One implementation: the table belongs in `crates/panel`, derived
  from the same data the existing firmware-table check uses, not typed in.
- Card 248 (firmware) will add steadier sub-levels below level 1 and change the dither.
  This card must read the level structure from the model so that 248 changes one place.
  It does not depend on 248 and should not wait for it.

## Deliverables

1. `crates/panel`: the aligned code for each level (lowest code reaching it), how far a
   given code is from its nearest level in sixteenths, and a test against the firmware
   table beside the existing one.
2. `crates/art` (`panel.rs` and wherever the hand-over quantises): a dark-end snap -
   a channel whose target is below a threshold level (start at 16, make it a named
   constant) goes to the nearest aligned level as the **last** step, after blending and
   anti-aliasing; channels above it are untouched and keep the 1008-step path. Decide,
   and say in the Log, whether this is a third `output.panel` value or a separate
   `output` flag; the default must not change any existing patch's pixels (golden tests).
3. The clocks patch (`crates/art/src/patches/clocks`): its dark ramp designed as a short
   list of level triples per 2.1.1 (per-channel alignment means the darkest shades lose
   hue control - choose them, do not compute them), using the new snap. Offer the owner
   two or three variants to look at in the Studio rather than one.
4. Optional, only if it falls out cheaply: the "buy levels with brightness" trade from
   2.1.1 as a worked number for clocks (its peak level today, the scale factor, the
   brightness that compensates, against card 136's floor and card 187's policy). A note
   in the Log is enough; do not build a mechanism.
5. `docs/design/generative-art-brief.md` 2.1.1 updated to name the real API.

## Acceptance

- `cargo test` and `cargo clippy --workspace --all-targets` clean; goldens for every
  other patch byte-identical.
- A host test that a clocks frame, after the snap, has no channel value below the
  threshold that is off-level by more than 2/16.
- The owner, within a few feet of the panel with the device dither on, sees the clocks'
  dim shades present and not blinking.

## Log

### 2026-09-21 - shaping note from the firmware session (card 248), folded into Context

Not new scope - a constraint on how deliverables 1-2 are built, so future firmware work
(card 248) has exactly one place to change:

1. The snap must take "the list of steady duty values" as **data** read from `crates/panel`
   (derived from the same table the existing firmware-table check uses), rather than
   `crates/art` assuming duties land every 16 sixteenths. 248 will probably add one or two
   steady sub-levels below level 1 (a half level near sRGB 21-22, maybe quarters near 12 and
   28), and that set may become a function of runtime brightness. This card does not build
   for that - it only avoids baking `* 16` arithmetic into `crates/art`, and instead calls
   into a `crates/panel` function (built from [`DITHER_PHASES`], not a literal `16`) that 248
   is free to change.
2. 248 will bit-reverse and likely shorten the dither cycle, so the threshold level (16) may
   come down later - kept a named constant per the card's own instruction.
3. `crates/panel/src/model.rs`'s existing constants (`DEVICE`, `DITHER_PHASES`, the steps
   count) are not touched by this card, only added beside - 248 changes those at its own end
   and rebases onto this merge.
4. Fact to have right: `output.panel: bit_planes` is sender-side only choice of which codes
   to send; the device's temporal dither stays on in both Studio modes. `bit_planes` looks
   steady because nearest-level codes happen to have tiny remainders, not because the device
   stopped dithering.

### 2026-09-21 - worktree was behind main

The worktree this card started in was at commit 127c829 (card 178, several commits before
the board was pruned to just 188/187/199). Re-branched from `main` at 6bdcc1c per the
coordinator's instruction; `card/188-held-dark-shades` now starts there.

### 2026-09-21 - deliverable 1: the aligned-code table in `crates/panel`

Added to `crates/panel/src/model.rs`, beside the existing `DEVICE`/`DITHER_PHASES`
constants and their firmware-fixture test:

- `duty_16ths(v: u8) -> u32` - an sRGB8 code's duty in sixteenths of a level, recovered
  from `DEVICE.emit1(v) * DEVICE.max()` rather than typed in from the brief or the
  firmware. New test `duty_16ths_is_the_firmwares_srgb_to_q` checks it against the
  `FIRMWARE_SRGB_TO_Q` fixture **to the integer**, not `DEVICE.emit1`'s 1e-6: an aligned
  code is chosen by comparing duties directly, and a float wobble at a boundary would
  pick the wrong one.
- `nearest_level(duty_16ths) -> (level, signed offset)` - any duty's nearest whole level
  and how far off it is, round-half-up (matches the firmware's own `SRGB_TO_Q` rounding).
- `AlignedLevel { level, code, offset_16ths }` and `aligned_levels(max_level) -> Vec<_>` -
  the lowest sRGB8 code that *reaches* each level (`duty(code) >= level * DITHER_PHASES`,
  the brief's "lowest code that lands on"), and how far past the level it actually sits.
  Reverse-engineered the exact rule from the brief's own 17-value list: it is **not**
  "nearest by rounding" (that would put sRGB 21, duty 8, at level 1 - it is not, per the
  existing `dark_srgb_is_no_longer_off`/`BitPlanes` tests) - it is "smallest code whose
  duty has reached or passed the level's floor". Checked every one of the 17 listed
  values (0, 34, 50, ... 138) by hand against `FIRMWARE_SRGB_TO_Q` before writing the
  function, including the worst case (level 13, sRGB 126, 2/16 short) - the new test
  `aligned_levels_match_the_brief` pins all 17.
- `nearest_level_agrees_with_the_aligned_table`: every aligned code's own nearest level is
  the level it was chosen for (never the one below) - the two functions can't disagree
  about an aligned code.

No literal `16` in any of it - `DITHER_PHASES` throughout - per the firmware session's
shaping note above.

`cargo test -p screeny-panel --release`: 17/17 green (was 15). `cargo clippy -p
screeny-panel --all-targets`: silent.

Deliverable 1 is `screeny_panel::{duty_16ths, nearest_level, AlignedLevel,
aligned_levels}`, re-exported from the crate root.
