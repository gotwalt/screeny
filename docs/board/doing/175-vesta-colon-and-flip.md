---
id: 175
title: vesta - the colon is a face's glyph, and a Flip button
type: build
hardware: no
depends: [174]
owner: worker-175
branch: card/175-vesta-colon-and-flip
---

## Goal

Card 174 made vesta's numerals a choice and four of the six faces exactly crisp:
inside a module there are two colours, full ink and true black, and nothing between.
The **colon** is not part of any face. It is still two drawn circles
(`COLON_R = 1.15`, `sample()` in `patches/vesta/mod.rs`), so its edge is
anti-aliased - and that is the whole difference between a 2-colour picture and the
4-colour frame `snapshot --time 04:56` actually reports for a pixel face.

## Context

- Two LEDs of the colon are full and the rest of each dot is a ramp. At the sizes
  involved a "circle" of radius 1.15 LEDs is a 2 x 2 block with soft corners; drawing
  it as a 2 x 2 block of LEDs would look the same across a bedroom and cost two
  palette entries fewer.
- `size` < 1 has to keep working: the colon is scaled with everything else, and below
  1 nothing is crisp anyway. So this is about `size` 1.
- The tidy version is that a face carries a `':'` glyph and the patch asks the face
  for it, which is exactly what `crates/art/src/faces/` was shaped for (glyphs are
  looked up by `char`, and a face that has not got one draws nothing). Terminus,
  Spleen and Dina all have a real colon in their BDFs; Micro Grotesk has none, which
  is why vesta draws its own today. A face without a `':'` would fall back to the
  drawn dots.
- Not urgent and not a bug: 4 colours is far inside `GUARANTEED_PALETTE` and the
  frames are 370-400 bytes of a 1464-byte budget. It is worth doing for the same
  reason the numerals were: the panel is a grid and the picture should land on it.

## Deliverables

- `':'` extracted per face by `tools/art-faces.py` where the font has one, and vesta
  asking the face for it; the drawn dots stay as the fallback.
- `a_pixel_face_is_exactly_crisp_at_size_one` extended past the module boxes to the
  whole panel, for the faces that carry a colon.

## Acceptance

`snapshot vesta --time 04:56 --set font=1` logs `colours=2`.

## Log

### Amendment (the orchestrator, 2026-09-20)

From card 184's report: a rotation can only be watched by waiting for a minute.
So vesta also gains **one action**, `flip`, labelled "Flip", the way the clock
patches offer "Compose another" and "Move on" (`Patch::playing` / `Playing` /
`Patch::act`; the Studio draws an action as a button in "Now playing" and posts
`patch_act`). Pressing it starts a rotation **now**, in whatever `flips` mode is
selected, landing on the current time. In "changed cards only" it must still do
something visible - a full rotation is the honest reading of a button marked
Flip - and what was chosen must be said. It must not disturb the next minute's
own rotation: if it is pressed during one, either ignore it or queue it, and say
which. A pinned-time render must stay byte-identical when the button is never
pressed. It needs a test. The Studio's page should need no change for an action
to appear - check by reading `crates/studio/ui/picture.js`, do not edit
`crates/studio`.
