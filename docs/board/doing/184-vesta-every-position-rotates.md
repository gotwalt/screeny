---
id: 184
title: vesta - on the minute, every position makes a full rotation
type: build
hardware: no
depends: [174]
owner: worker-184
branch: card/184-vesta-rotation
---

## Goal

The owner, 2026-09-20, with the new faces on the panel: "For vesta, let's do an entire
rotation of every position on minute change. I think the fun of a flipboard is that it
flips."

When the minute turns, **all four modules** run through their whole drum and land on the
new time - the ones whose numeral did not change too. Once a minute the board does what a
flipboard is for.

## Context

- Today (`crates/art/src/patches/vesta/`, cards 155 and 174): only modules whose numeral
  changes flip. With `cascade` on a module passes through the numerals between; without
  it, it falls once. The hours' tens has a three-card drum (blank/0, 1, 2); the others
  count in their own ranges. A card's fall is `flip` = 0.2 s: six frames at 30 fps, under
  gravity, with a small settle. `:59 -> :00` on the minutes' tens is about a second.
- What a real board does, and what to take from it: every module carries the **same**
  drum and a full rotation takes every module the same time, which is why a Vestaboard
  refresh is a wave of identical clatter and not four different little animations. Cards
  fall **fast** - ten or more a second - and the next card is already falling before the
  last has landed: several cards are in the air at once near the axle. Modules are not in
  step: they start a few tens of milliseconds apart and drift, which is most of the charm;
  and they *stop* at different moments because each has a different distance to go - the
  board resolves into the new time module by module.
- So the design questions, to settle by LOOKING at strips and saying what was tried:
  1. **The drum.** One drum for every module - blank, 0-9 (eleven cards; perhaps the colon
     or a few letters later, which is what `crates/art/src/faces/` looks glyphs up by
     `char` for) - so a full rotation is the same length everywhere, against today's
     per-position drums. With one drum, "a full rotation and land on the new numeral" is
     eleven cards plus the distance to the target (or exactly one revolution ending on the
     target - choose, and say why). The hours' tens still shows blank for a leading zero.
  2. **Speed.** Eleven-plus cards at 0.2 s each is over two seconds of every minute, and it
     would look like slow motion. A rotation wants its own, faster card time (try 0.08-0.12
     s a card: 3 frames at 30 fps is the floor at which a fall still reads as a fall, not
     a flicker) and overlapping falls - the next card released before the last lands -
     which `flap.rs`'s single-falling-card model does not do today. Show the six-frame
     single fall against the fast overlapped one. Consider easing the *last* two or three
     cards back to the slow, readable fall so the landing is the satisfying part.
  3. **Stagger.** Per-module start offsets and slightly different card times (a few
     percent, fixed per module - they are mechanical parts, not random per minute), so the
     four are never in lock-step. The order they resolve in is part of the look: left to
     right, or minutes first? Try it.
  4. **How long in all.** A guess at a good total: 1.2-1.8 s from first card to last
     landing. A bedroom clock that clatters for three seconds a minute is too much; half a
     second is not a rotation. Make it a parameter and pick a default by eye.
- This is a **night clock**: a full rotation lights more of the panel, more often, and the
  lit edges of four fast cards are the brightest thing the patch draws (card 155 flagged
  `EDGE_BOOST`). Measure peak and mean APL over the rotation and over a whole minute,
  against today's; consider dimming the lit edge during a fast rotation. It must stay a
  quiet thing in a dark room - the motion is the event, not a flash.
- Make it a named choice, because he may want the old behaviours back at 3 a.m.:
  `flips`: "full rotation" (the **default** now), "through the numerals between" (today's
  cascade on), "changed cards only" (today's cascade off). It replaces the `cascade`
  toggle; an old named setting or state that carries `cascade` must still load (card 151:
  unknown parameters are dropped quietly - check that is what happens and that the result
  is sensible).
- Everything else holds: pinned-time renders are byte-identical (`--time 09:59:58 --at N`
  recipes - update the README's frame table for the new timing, and the test that checks
  it), the palette stays within 32 and exact on every frame INCLUDING with several cards
  in the air (check `bytes=` at the busiest frame), zero green and blue at the default
  hue, every face works (pixel faces resampled mid-fall are fine), the blank card is a
  card like any other, `pace` still compresses a minute for watching it.

## Deliverables

- The rotation in `flap.rs`/`mod.rs`, the `flips` choice, tests (every module ends on the
  right numeral for every minute of a day, in all three modes; a rotation ends within its
  stated time; the modules are not in step; palette/exactness at the busiest frame; pinned
  renders identical).
- Strips in the scratchpad's `vesta-faces/` named `184-...`: a whole rotation at the
  default as a contact strip of every frame (about 45 frames - several rows); the same at
  two other speeds; single slow fall against fast overlapped falls; the last half second
  (the landing) enlarged; one at 23:59 -> 00:00 (the hours' tens landing on blank).
- In the Log: APL over the rotation and over the minute, bytes at the busiest frame, the
  timing chosen and what else was tried; "Open with the owner".

## Acceptance

The owner watches a minute turn on the panel and it is fun.

## Log
