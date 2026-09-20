---
id: 155
title: vesta - a low-light split-flap clock for the night
type: build
hardware: no
depends: [150, 162]
owner:
branch:
---

## Goal

The owner, 2026-09-20: "let's design a simple low light clock for night time. Defaults to
red numerals, fill only maybe half the pixels, render in 3d as if it were a vestaboard,
where the numerals animate vertically on change. Maybe the patch is called vesta and we
work to explore that visual idea."

A new patch, `vesta`. This card is a **first version to look at and argue with**, not a
finished design: build the idea honestly, expose what is a matter of taste as parameters
he can move on the page, and come back with pictures.

## Context

- **Start after card 150 has merged** (pieces become patches): write it as a patch from
  the first line. Copy the shape of `crates/art/src/patches/clocks/` (a CPU patch:
  `Frame::supersample`, an indexed frame with a small exact palette, `Ctx::now` as the only
  clock, `hours24` and `offset` parameters as the other clocks have them).
- **What a split-flap is, for whoever draws it.** Each character is a stack of flaps on a
  horizontal axle through its middle. At rest you see the top half of the character on the
  upper flap and the bottom half on the lower one, with a thin dark seam between them -
  the seam is the signature, keep it. On a change the upper flap falls forward about the
  axle: behind it the top half of the *next* character is already showing; the falling
  flap carries the old top half on its front until it is edge-on at 90 degrees, and the
  new bottom half on its back as it comes down to cover the old bottom half. A real
  module gets from 3 to 7 by flipping through 4, 5 and 6.
- **"In 3D" at 64 x 32.** A falling flap is a rectangle rotating about a horizontal axis:
  its height is foreshortened by cos(angle), under perspective its free edge comes towards
  the viewer and gets a little wider, and - this is what sells it at this size - its
  **brightness changes with the angle** as it turns through the light, with the flap behind
  it falling into its shadow. All of it is analytic per sample inside
  `Frame::supersample`; no GPU scene is needed, and the CPU indexed path is what keeps a
  red-only picture exact on the wire. Numerals should be vector strokes or a supersampled
  glyph, not a 1-bit bitmap, or a foreshortened flap will shimmer.
- **Low light is the brief, and it has a trap.** Card 102 measured the panel's dark end:
  codes under about sRGB 38 are made of few refreshes and large areas of them *sparkle*
  faintly, which in a dark bedroom is exactly what one would see. So dim is better reached
  by lighting **few** pixels at a **moderate** level than many at a very low one; by pure
  red (one LED die a pixel, and kind to night vision: keep green and blue at zero unless a
  parameter asks); and by the panel's own brightness control, which costs no depth (card
  066). The flap *bodies* should therefore be black or all but black - do not paint 24
  dark-grey cards, they will shimmer - and the 3D has to read from the numerals' own
  shading, the seam, and the moving flap's edge. The average picture level is on the page's
  meter: aim for a resting APL of a few percent and say what it is.
- **Size: spend the pixels.** The owner first said "fill only maybe half the pixels" and
  then withdrew it the same afternoon: "my half the panel assertion is faulty. We might
  need most of the panel in order to make the flipping operation really look right. And I
  think I'd rather spend the pixels than be artificially constrained." So the flip is what
  sizes the clock: make the modules as large as `HH:MM` allows on 64 x 32 - four modules
  around 13-14 LEDs wide and most of the 32 tall, a narrow colon, a pixel or two of true
  black between modules so each reads as its own flap - because a falling flap only looks
  like one if it has enough rows to foreshorten through (a 10-row half-flap passes through
  10, 9, 7, 5, 3, 1 rows; a 6-row one is a blink). Low light then comes from *what* is lit,
  not how little of the panel: pure red, a moderate numeral level, black flap bodies, thin
  strokes if they read better than heavy ones, and the panel's brightness. Keep `size` as a
  parameter so the modest version can still be looked at, and keep the halftone `fill`
  parameter (off by default) as a cheap way to halve the light without shrinking anything.
- Layout to start from: `HH:MM`, four flap modules and a colon, 24-hour by default like
  the other clocks. No seconds. Whether the colon blinks (no: nothing in a bedroom should
  blink) is a parameter at most.
- A patch cannot set the panel's brightness today; brightness is the panel's. Whether a
  night patch should be able to *ask* for one, or a named setting (card 151) should carry
  one, is a question for the owner - write it under "Open with the owner", do not build it.
- Card 162 makes this designable: `screeny-art snapshot vesta --time 21:12 --out x.png` is
  the settled picture, and `--time 21:12:58 --at 3` (or wherever the flip falls) is a
  mid-flip frame, the same PNG every time. Use them for every before/after.
- The limiter (`output.limiter`, APL cap 0.4) will never bite on this patch; leave it.

## Parameters to expose (starting values are the worker's; the point is that they move)

`hue` (default red, 25-30 deg OKLCH or pure sRGB red - try both and say which looks
cleaner in the snapshot), `light` (numeral level), `size` (how much of the panel the clock
takes; default: as much as fits), `fill` (solid ... halftone), `flip` (seconds a flap takes to
fall), `cascade` (flip straight to the new numeral, or through the ones between, as a real
module does), `tilt` (how far off-axis the viewer is: 0 is flat-on, more shows more of the
falling flap's perspective), `seam` (the dark line's weight), `hours24`, `offset`.

## Deliverables

- `crates/art/src/patches/vesta.rs` (or a `vesta/` module), registered; README entry.
- Tests in the style of the clocks': the right numerals for a given `Ctx::now`; a flip
  starts when the minute turns and ends within `flip` (times `cascade` steps); the palette
  stays within `GUARANTEED_PALETTE` so every frame is exact; the resting picture contains
  no green or blue at the default hue; resting APL under a stated bound; pinned-time
  renders are byte-identical (card 162's test pattern).
- In the card Log: the snapshots that were looked at (settled, three mid-flip angles, the
  halftone variant, two sizes), as file names under the scratchpad plus a one-line
  description of each - and the same PNGs copied somewhere the orchestrator can read them
  to show the owner.
- "Open with the owner": everything that is taste.

## Acceptance

On the panel, at night, by the owner's eye. Before that: the orchestrator looks at the
snapshots and the mid-flip sequence reads as a flap falling, not as a wipe or a squash.

## Log
