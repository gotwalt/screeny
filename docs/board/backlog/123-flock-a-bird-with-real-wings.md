---
id: 123
title: Flock - a bird with real wings, for when it is drawn big
type: build
hardware: no (the owner judges it on the panel through the Studio)
depends: [122]
owner:
branch:
---

## Goal

The owner, 2026-09-21, after card 122 let him draw a handful of big birds: "this is better,
but the expanded bird shapes don't have natural looking wings - can we improve the 3d model
of the bird". At `size` 2-3 a bird spans 10-20 panel pixels and what is there is a stick
figure. Give it a wing.

## Context

- `crates/art/src/patches/flock/mod.rs`, `draw_birds` (line ~511): a bird is five projected
  points - nose, tail, shoulder, two wing tips - joined by three strokes of clamped width.
  The wing is ONE straight segment from the shoulder to a swept-back tip, raised and lowered
  by a single dihedral angle; the comment says it itself: "Tips swept back: the only thing
  standing in for a wing's shape." That was right for a 3-pixel bird (card 168) and is what
  looks unnatural at 15.
- What makes a wing read as a wing, roughly in order of how much it buys at this resolution:
  1. **Two segments with a wrist.** Inner wing (shoulder -> wrist) and hand wing (wrist ->
     tip). On the downstroke the wing is nearly straight and swept slightly forward; on the
     upstroke the wrist folds - the hand wing sweeps back and trails - so the span visibly
     shortens. The tip LAGS the inner wing in phase. This asymmetry is the single biggest
     cue; card 168 already makes the downstroke quicker than the upstroke, keep that.
  2. **Area, not a line.** A wing has chord: broad at the root and through the wrist,
     tapering to the tip, with a straighter leading edge and a curved trailing edge. Fill
     it (triangles / a polygon through the coverage buffer) rather than stroking a line
     with a clamped width. Seen edge-on a wing should thin to almost nothing; seen from
     above or below (a banking bird) it should be at its broadest - that comes free if the
     wing is a real surface in the bird's frame and is projected.
  3. **A body and a tail.** A tapered body (fatter at the chest, a short head/beak forward
     of the shoulder) and a small tail fan that spreads a little in a glide or a hard turn.
  4. **Glide pose**: wings held slightly raised with the hand wing swept back a touch, the
     gull "M" seen head-on. `bird.glide` already exists.
  5. Optional, only if cheap and it reads: underside slightly darker/lighter than the top
     (the patch has two tone schemes - respect them), so a banking bird flashes.
  Look at reference for a generic passerine/starling-to-gull silhouette in your own
  knowledge; do not model a particular species' feathers. It is a silhouette.
- **Level of detail by on-screen span.** At `size` 1 and 55 birds most birds are 2-5 pixels:
  the new model must collapse gracefully to something as good as today's there (the owner
  said "this is great" about that look). Decide by projected span, not by the `size`
  parameter. The default picture should stay as close to today's as honest work allows;
  if the default frames change, say exactly how and show before/after.
- Read `Coverage` (the anti-aliased coverage buffer `draw_birds` writes into) before
  designing: what primitives it has, how it supersamples (`SUPERSAMPLE` = 3), how far
  birds are sorted and hazed. Add a filled-triangle/polygon primitive there if there is
  none; keep it small and tested.
- Nothing in `sim.rs` should need to change: this is drawing. If the wrist fold wants a
  per-bird quantity the sim does not have (e.g. flap amplitude tied to climb or speed),
  derive it from what `Bird` already carries before adding state.
- Cards 168, 177 and 122 in `docs/board/done/` are the history; 122's Log says why `size`
  is a draw-time lens and what happens with few birds up close.

## Deliverables

- The new bird in `crates/art/src/patches/flock/` (a `bird.rs` for the model is welcome if
  `mod.rs` is getting long), with the level-of-detail rule.
- Tests: the existing flock tests green (`every_frame_goes_out_exactly` matters: frames must
  still encode exactly within the datagram budget with big filled birds on every backdrop;
  the brightness limiter must still not be defeated by large light birds); a test that the
  wing's drawn span is shorter mid-upstroke than mid-downstroke; goldens of every other
  patch byte-identical.
- **Pictures for the owner**, fixed seed, outside the repo tree: a flap cycle contact sheet
  of ONE big bird (8-12 consecutive phases, `birds=3..6`, `size=2.5..3`) before and after;
  the same for a banking turn and a glide; and the 55-bird default before and after. Look
  at every one yourself with the Read tool, iterate until the wing reads as a wing in the
  STILL frames and the cycle reads as flapping when you step through it, and say honestly
  in the report what still looks wrong.
- `crates/art/README.md`'s Flock section updated.

## Acceptance

The owner, with `birds` around 6 and `size` around 2.5 on the panel, sees birds whose wings
bend and beat like wings.

## Log
