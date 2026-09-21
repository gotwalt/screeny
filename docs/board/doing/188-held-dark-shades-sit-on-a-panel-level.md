---
id: 188
title: Art - held dark shades sit on a panel level: the aligned-code table in the panel model, a dark-end snap that leaves the brights alone, and the clocks patch on it
type: build
hardware: no (the owner looks at the result on the panel through the Studio)
depends: []
owner:
branch:
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
