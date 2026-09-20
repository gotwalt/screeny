---
id: 102
title: Reconcile the art system's panel model and colour budget with the measured device
type: build
hardware: no
depends: [100]
owner:
branch:
---

## Goal

`crates/art` was built from the first version of the art brief. The brief has since been
rewritten from measurements. Bring the preview and the pieces' working rules up to
date, so the studio shows what the panel shows and pieces stop being stricter than
they need to be.

## Context

`docs/design/generative-art-brief.md` sections 1, 2.1, 2.3 as they are now, against
`crates/art/README.md` "Provisional assumptions". Known gaps:

- **Dark end.** The preview hard-quantises to 64 linear levels (`panel.rs`), with
  32/16-level options for "dimmed by scaling". The device now dithers in time (darkest
  visible about sRGB 6, not 34) and brightness no longer costs depth. The 32/16 options
  are obsolete; the dark end needs a model of the time-averaged result (and perhaps its
  faint sparkle under sRGB ~30).
- **Pieces avoid the darks by rule.** `overland` cuts any palette colour below OKLCH
  L 0.3 to black; the clock pieces start their ramps at L 0.32. With temporal dithering
  "slow fades to black are now usable". Revisit with the owner: some of this is now
  style rather than necessity.
- **Colour budget.** Pieces keep to 32 colours because that was the only exact case.
  `PAL8_LZ` makes up to 256 exact when the index image compresses, and spatial
  coherence, not colour count, is the real cost. `Palette`, the studio's Colours and
  Frame size meters, and the advice in `crates/art/README.md` should say so. Best done after
  101, with the real encoder answering.
- **Frame rate.** The art system assumes 60 fps on the owner's instruction; the brief says 30
  is the design rate and WiFi sets the ceiling. Decide what the sender does when 60
  does not get through, and show drops in the studio once 101 exists.

## Deliverables

Updated `panel.rs` / `pipeline.rs` / studio controls; pieces adjusted where the owner
wants; `crates/art/README.md` assumptions table brought up to date.

## Acceptance

Studio preview of the test card agrees with a camera capture of the panel showing the
same frames (capture by the orchestrator), to the eye, in the dark ramps.

## Log

### Note from card 066 (2026-09-19)

What the device's dimming actually is, in case it saves 102 the reading. The
firmware quantises at full depth (`firmware/src/gamma.rs` is the sRGB EOTF and
brightness is deliberately not in it) and then dims by shortening the
output-enable window: `display::slots_for(b) = (b * 25 + 127) / 255` lit slots
out of `MAX_OE_SLOTS = 25`, linear in duty and so linear in light. All 64 duty
levels exist at every brightness; only the light goes away.

Card 066 put that in `screeny_panel` as `MAX_OE_SLOTS`, `oe_slots(b)` and
`oe_light(b)`, and `Lut::new` now quantises first and scales the emitted light
after. So "dimmed by scaling" is not a thing the panel does at any brightness,
and `crates/art`'s 32/16-level options are not a dim room - they are the old
bug. If 102 wants "what a dim room looks like", it is 64 levels times
`oe_light(brightness)`, not fewer levels. The pre-020 order is still available
as `Lut::value_scaled` if a comparison is wanted.

Also worth knowing for any brightness control in the studio: the control has 25
steps, not 256, and brightness 1..=5 lights zero slots (card 136).
