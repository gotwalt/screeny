---
id: 128
title: Flock - the underside flash does not fire, because six ink levels cannot hold it
type: build
hardware: no (the owner judges it on the panel through the Studio)
depends: [124]
---

## Goal

Card 123 drew a wing's underside a sixth brighter than its top (`UNDERSIDE` = 0.16 in
`flock/mod.rs`, inverted for the dusk scheme) so a bird rolling through a turn would
flash. Card 124 gave it something to flash *with*: the drawn lean reaches 30-37 degrees,
and the nearest big bird's presentation - how much of its wing plane the camera can see
- went from 0.0-0.5 through a turn to 0.3-0.7. The geometry is now right and the flash
still hardly shows.

The reason is tone, not geometry. Over the sky backdrop a bird has **six** ink levels
([`INK`]), because the palette is 14 sky bands x 6 and that is what 256 entries allow.
A sixth of one level, plus the haze that already scales the ink with distance, plus a
blue-noise dither threshold, is very often the same index for both faces of the bird -
so the two wings quantise to the same grey and there is no flash. With the sky off there
are twelve levels and it should show better; nobody has looked.

## Context

- `crates/art/src/patches/flock/mod.rs`: `UNDERSIDE`, `INK` / `INK_DARK`,
  `ink_levels`, the `palette` construction and the per-wing `lit` in `draw_birds`.
- Card 124's Log has the presentation numbers and says this is the one thing left that
  the lean did not fix. Card 123's Log says what the flash is for.
- Things to weigh, all of them cheap to try and only worth having if they *read*:
  - a bigger `UNDERSIDE` - it is 0.16 because it was meant to be a hint, and a hint
    inside one sixth of the range is nothing;
  - fewer sky bands in exchange for more ink levels (the palette is a rectangle and the
    budget is 256 entries; the sky's bands are what the Bayer dither is hiding anyway);
  - letting the *lean itself* drive a little extra ink rather than the face normal,
    which is a lie but might be the readable one;
  - leaving it alone and taking the flash out, if the honest answer is that six levels
    over a lit sky cannot carry it. That is a real outcome and worth writing down.
- Measure before deciding: for the nearest big bird through a turn, how many *index*
  steps separate the two wings' ink, per backdrop and scheme. If it is zero, no amount
  of geometry will help.

## Deliverables

- Either a flash the eye catches at `birds` 6, `size` 2.5 on both the sky and the black
  backdrops, or evidence in the Log that the palette cannot hold one and `UNDERSIDE`
  removed rather than left as a number that does nothing.
- Pictures for the owner, fixed seed, outside the repo tree.
- `crates/art/README.md`'s Flock section updated either way.

## Acceptance

The owner sees a bird's underside catch the light as it leans through a turn - or reads
why it cannot and agrees to let it go.
