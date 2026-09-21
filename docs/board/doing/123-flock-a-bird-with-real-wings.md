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

### 2026-09-21 - what is drawn now

A bird, in fractions of its drawn wingspan `S`:

| part | where |
| --- | --- |
| nose / chest / hip / tail tip | `+0.30 / +0.06 / -0.16 / -0.40` along the heading |
| shoulder | `+0.11` forward, `0.05` off the centre line |
| wrist | `0.45` of the semi-span out, `-0.02 .. +0.10 S` behind the shoulder with the fold |
| tip | a further `0.55` of the semi-span, swept back `0.34 .. 1.05` rad with the fold and drawn in a tenth |
| wing chord | `0.19 S` at the root, `0.115 S` at the wrist, nothing at the tip |
| tail fan | `0.090 .. 0.150 S` half-width, with `glide` and `|roll|` |

Angles: inner dihedral `0.62 * warp(phase)`, hand dihedral `1.3x` that on `phase - 0.55`,
fold `smoothstep(-0.35, 0.75, d/dphase warp(phase - 0.55))`. Glide holds the inner wing
at `+0.24` rad and the hand at `-0.30`, and the fold at `0.34`.

Stroke weights: the chest keeps card 168's `(span * 0.17).clamp(0.42, 1.30)` exactly, and
the neck (`0.60x`, floor 0.40) and the tail boom (`0.42x`, floor 0.38) are fractions of
it that floor at about the same sub-pixel width - so a distant bird is the same even dash
it was. The wing spar keeps `(span * 0.13).clamp(0.38, 1.05)` and the hand is `0.70x` of
that (floor 0.36), because a wing's leading edge is thicker at the shoulder.

**Level of detail**: `AREA = (5.0, 9.0)` LEDs of projected span, smoothstepped, scaling
the chord and the tail fan. The default flock's biggest bird runs a median of 6.4 LEDs
and 7.7 at p95 (`ten_minutes_of_flight`), so at the default only the nearest few birds
have any surface at all and the rest are exactly card 168's skeleton.

**The default picture**: 5-10% of the 2048 LEDs differ from before (113, 118, 161 and 199
LEDs at t = 12, 20, 28 and 36 s, seed 7), which is most of a bird pixel here and there
and nothing else - the sky is untouched. The flock reads exactly as it did: same density,
same marks, same character. What changed is that a near bird is slightly narrower through
its upstroke and has a hint of wing behind its leading edge. See `compare-default.png`.

**Panel safety.** Black backdrop with six big white birds, 1800 frames: peak APL **3%**,
the limiter never below x1.00, worst frame 966 of 1464 bytes, 0 lossy. A lit sky with the
same six peaks at 24% for the dusk scheme, which is what the dusk sky alone already costs.
Filled wings gave the panel nothing to worry about - if anything less, because the fold
shortens the span through half of every beat. At the far corner of both controls
(`birds 150, size 3.0`) the sky picture peaks at **1462 of 1464 bytes** against 1433 for
the same sweep before this card; still exact, but there is no headroom left there. That
and the flock's tiny roll are **card 124**.

**Tests.** `a_filled_triangle_covers_its_own_area` (96.00 of 96 square LEDs either
winding; a degenerate triangle draws exactly 0),
`the_wing_is_narrower_going_up_than_coming_down` (**12.3 LEDs mid-upstroke against 16.0
mid-downstroke**, projected through a camera below the bird, at the two extremes of the
wing's own vertical speed), `a_distant_bird_has_no_surface_left`, and
`every_frame_goes_out_exactly` now flying every backdrop twice - at the defaults and at
six birds drawn as big as `size` goes.

`cargo test -p screeny-art --release`: 118 passed. `cargo clippy -p screeny-art
--all-targets`: silent, no `#[allow]` added. No golden of any other patch was touched;
nothing outside `crates/art/src/patches/flock/` and `crates/art/README.md` changed, and
`sim.rs` is byte-identical.

**Pictures** in the session scratchpad under `flock-wings/`, before over after in each
pair: `compare-default.png` (the 55-bird default, four moments), `compare-glide.png`,
`compare-flapbig.png`, `compare-flap.png`, `compare-bank.png`, and the size-3 singles
`before/bigsheet.png` / `after/bigsheet.png`.

**What still looks wrong**, honestly:

- **The body is still too long for the wings.** Real gulls are 0.44 of a wingspan nose to
  tail; this bird is 0.70, because that is what card 168 chose and it is what makes a
  three-pixel bird read as a dart. Seen nearly head-on - which is the commonest view,
  because the camera flies with the flock - the body carries the shape and the bird reads
  a little like a paper dart.
- **The wing root runs into the body.** The root chord reaches back to about the hip, so
  there is no gap between the wing's trailing edge and the body and the whole thing is
  one mass. At 16 LEDs there is no room for a gap, but it is why a spread bird seen from
  below is closer to a cross than to a bird.
- **The wrist kink barely reads in the common views.** It is unmistakable in the planform
  and head-on (the "smile" and "frown" of a beating wing), and nearly invisible from the
  side. What actually carries the flap in most frames is the span shortening and the tip
  lagging, not the bend.
- **The underside flash almost never fires**, because the flock barely rolls. Card 124.
- **No head.** The neck is a thinner stroke and that is all; there is no distinguishable
  head at any size the panel offers.
- At 3-5 LEDs the bird is a dash or a shallow V, exactly as before. That is the design,
  not a gap - but it means most of the default picture gets nothing from this card.
