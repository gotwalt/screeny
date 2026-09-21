---
id: 123
title: Flock - a bird with real wings, for when it is drawn big
type: build
hardware: no (the owner judges it on the panel through the Studio)
depends: [122]
owner: worker (opus)
branch: card/123-flock-bird-wings
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

### 2026-09-21 - claimed

Branch `card/123-flock-bird-wings`, cut from `main` at 7f95e5e. Read `CLAUDE.md`,
`docs/README.md`, this card, cards 168, 177 and 122 in `docs/board/done/`,
`docs/design/generative-art-brief.md`, then `crates/art/src/patches/flock/` in full.

### 2026-09-21 - the "before" pictures, and finding moments worth photographing

Rendered the before set first, from the untouched code (`before/` in the scratchpad):
a 16-frame flap cycle, a glide and a banked turn at `birds 6 size 2.5 seed 7`, plus the
55-bird default at four moments. Confirmed by eye what the card says: at 16-18 LEDs the
bird is a three-stroke stick figure - a fat body dash with two straight rods off it, an
asterisk rather than a bird.

Finding the moments took a throwaway `#[ignore]`d scouting test (never committed): it
flew seed 7 with 6 birds and printed, per frame, the biggest in-frame bird's projected
span, roll, glide and *presentation* (how much of its wing plane the camera can see).
Two things fell out of it that shaped the pictures:

- **The flock hardly banks.** At the shipped `calm` 0.90 the largest roll any big bird
  reaches in 100 s is **7 degrees**; even at `calm` 0.15 / `wild` 0.9 it only reaches
  **13**. Roll is `atan(lateral / g)` and the turn rate is deliberately gentle, so a
  "banking turn" picture has to be taken at a low `calm` and is still a shallow lean.
  The banking sheet is therefore rendered with `--set calm=0.15 --set wild=0.9` and
  says so.
- **Most big birds are seen nearly edge-on** (presentation 0.1-0.35). A bird's planform
  is the view that shows a wing best and it is the rarest one, which is exactly why the
  wing has to read from the side and head-on as well.

### 2026-09-21 - the model

New `crates/art/src/patches/flock/bird.rs`: the bird as a shape, built in its own frame
and handed back in world coordinates so the camera does all the foreshortening. Two
segments per wing with a wrist; the hand wing beats 1.3x further than the inner one and
**lags it by 0.55 rad of phase**, which is the wave running out along a real wing. The
fold is driven by the *derivative* of card 168's warped beat, lagged the same way -
`smoothstep(-0.35, 0.75, rate(phase - LAG))` - so the hand wing is swept back 60 degrees
and drawn in through the quick half of the beat and nearly straight through the slow
half. A first attempt drove the fold from the wing's *height* instead and was nearly
useless: height alone folds and unfolds symmetrically, and it is the asymmetry that
reads (mid-upstroke and mid-downstroke came out 0.54 vs 0.49 of full fold; with the
derivative they are 0.99 vs 0.00).

Each wing carries chord - a filled surface from a leading-edge spar back to a trailing
edge, broad at the root, narrower at the wrist, nothing at the tip - which needed a
filled-triangle primitive in `Coverage` (`mod.rs`), anti-aliased off the signed distance
to the nearest edge so its edges match the strokes'. The body is three strokes of
falling width (chest, neck, tail boom) and the tail is a small fan triangle that spreads
with `glide` and `|roll|`, both of which `Bird` already carries: **nothing in `sim.rs`
changed.**

**Level of detail by projected span, never by `size`**: `AREA = (5.0, 9.0)` LEDs,
smoothstepped, scales the chord and the tail fan. Below 5 LEDs the chord is zero and the
bird is card 168's bare skeleton, so the default picture keeps its look; the surface
grows out of the line instead of popping.

### 2026-09-21 - iterating by looking

Three rounds, judged on a throwaway turntable (also never committed: a temporary
`#[ignore]`d test that draws one bird from six directions - side, 45 below, directly
below, 45 above, three-quarter front, head-on - through eight wingbeat phases into a
PPM). Rendering into the real patch was not enough: the flock almost never presents a
bird broadside, so the planform could only be judged on a rig.

1. First proportions (root chord 0.26 span, body 0.74 span long, shoulder at 0.05):
   **a manta ray.** From below the two wings and the body fused into one solid lens with
   no waist and no wing shape at all. Rejected.
2. Narrowed the chord to 0.19/0.115 span, moved the wing root forward to 0.11 span and
   shortened the body to card 168's 0.70: readable as a bird in the folded phases, but a
   **plus sign** in the spread ones - the leading edge was perpendicular and the sweep at
   full extension (0.14 rad) was under two LEDs of offset.
3. Sweep at full extension up to 0.34 rad, and the wrist now reaches slightly *forward*
   of the shoulder when spread and is drawn back when folded
   (`INNER_SWEEP` -0.02 -> 0.10 span). That is what the card means by "nearly straight
   and swept slightly forward on the downstroke", and it is what finally made the spread
   phases read as a bird rather than an aeroplane.

The glide "M" needed doubling to be visible at all: at 0.12/-0.18 rad it was under a LED
of deflection across the semi-span and head-on the bird was a flat line. At 0.24/-0.30 it
is the gull M the card asks for.
