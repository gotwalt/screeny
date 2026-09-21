---
id: 122
title: Flock - fewer, bigger, closer birds, and parameters that explain themselves
type: build
hardware: no (the owner judges it on the panel through the Studio)
depends: [168, 177]
owner: worker (sonnet)
branch: card/122-flock-fewer-bigger
---

## Goal

The owner, 2026-09-21: "it's rendering too many birds, and too far away - the resolution of
the screen means that a lot of the detail gets lost. Can we modify the parameters so it's
possible to make fewer birds (current min is 30) and get closer (or maybe make the birds
bigger)? Also the parameters could use some better explaining - samples per axis is super
confusing." 64x32 pixels: a bird that is one or two pixels is a speck, not a bird.

## Context

- `crates/art/src/patches/flock/mod.rs` declares the parameters (line ~46): `birds`
  30..150 (default 55), `near` "How close the camera rides (m)" 2..16 (default 6),
  `samples` "Samples per axis" 1..6, and the rest. Read cards 168, 169 and 177 in
  `docs/board/done/` first - **169 is the important one: the camera could not ride close
  without scattering the flock** (the birds avoid it), and it was closed on its evidence.
  So "closer" may not be available just by lowering `near`'s minimum; "bigger birds" (a
  size parameter, or a narrower field of view - a longer lens makes the same birds bigger
  without the camera entering the flock) may be the honest lever. Find out which, by
  rendering.
- The flocking rules were tuned for 55 birds; at 5-15 birds cohesion, separation and the
  surges of card 177 may behave differently (a flock of 8 must still read as a flock, not
  eight strangers). Check, and scale what needs scaling with the count.
- The owner's saved settings on workbench must keep loading: changing a parameter's range
  or default is fine, renaming or removing an id needs the same care card 150/151 took
  (read how `state.rs` treats unknown or out-of-range params before you rename anything;
  prefer keeping ids and changing labels).
- Labels are the explanation the page shows (`param(id, label, ...)`); check how the page
  renders them and whether a longer hint/title exists (cards 163, 183). Every flock
  parameter's label should say what the person will SEE change, in plain words: e.g.
  `samples` is anti-aliasing quality - say "Smoothness of edges" or similar and consider
  whether it should be on the page at all (cost metrics are not the owner's concern; if a
  fixed value is right, fix it and remove the control, keeping old saved values loadable).
  `terrain` "Invisible geometry" and `calm`/`wild`/`lift`/`bank` deserve the same look.

## Deliverables

- `birds` goes down to a small flock (aim for a minimum around 3-5) and the flock still
  behaves; a way to make birds read larger on the panel (size and/or lens and/or a closer
  ride that works), with sensible ranges.
- New defaults only if they are clearly better on 64x32; if you change defaults, say so
  loudly in the report - the owner's tuned settings are saved, but "Default" would move.
- Every flock parameter relabelled for a person, not for the implementer.
- PNGs for the owner, rendered with `screeny-art snapshot flock --seed N --at S ...` at a
  fixed seed: today's default, and three or four candidates (e.g. 12 birds big and close;
  6 birds very big; 25 birds medium), with the exact `--set` values for each. Outside the
  repo tree; name the directory in the report. Look at them yourself and say which you
  would pick and why.
- Tests: the existing flock tests green; a test that the smallest flock still flocks (by
  whatever measure cards 168/177 used); goldens for other patches byte-identical.

## Acceptance

The owner can dial in a handful of large birds near the camera from the Studio page and
understands each control from its label.

## Log
