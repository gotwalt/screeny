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

### 2026-09-21 - claimed

Branch `card/122-flock-fewer-bigger`, cut from `main` at 2071788. Read the card twice,
`CLAUDE.md`, `docs/README.md`, cards 168, 169, 177 in `docs/board/done/` in full,
`docs/design/generative-art-brief.md`, card 163 (how `param`/`choice`/`toggle` and named
stops work), then `crates/art/src/patches/flock/{mod,sim,tests}.rs` in full and
`crates/studio/src/state.rs`'s `usable_params`/`remember` (read-only, per the card): an
unknown parameter id in a saved settings blob is simply ignored (`Params::set` returns
`false` and the value is dropped) - confirmed by the existing test
`a_value_this_build_cannot_use_becomes_the_default_and_the_rest_survive` - so removing a
control is safe for old saved values without any migration code.

Card 169's own closing note matters most: it says it was **closed by card 177's
evidence**, not fixed directly - at `near 3.0` with the full 55-bird flock the camera no
longer scatters it (spread 4.8-6.0 m, well under the 7 m the card asked for) and
foreground birds are already 8-10 LEDs. So the wire between "closer" and "scatters the
flock" is not the live bug the card worries it might be. I re-tested it myself before
touching anything (below) and found a *different* problem in the same territory: at a
**small bird count**, a close `near` makes the flock fall out of the camera's field of
view rather than scatter spatially - a new failure mode this card's own small-flock ask
exposes, not the one 169 fixed.

**Measuring before changing anything**, using the existing `fly()` test harness with a
temporary `#[ignore]`d experiment (never committed - deleted once it had answered the
question): three seeds x 3 minutes at combinations of `near` and `birds`, both well below
today's shipped ranges (temporarily lowering the param mins to test them, then reverting).
Findings:

- **The camera's personal-space wall holds `nearest` almost flat.** Across `near` 0.7 to
  6.0 at a fixed small bird count, the *nearest bird's* median distance barely moves
  (about 4.7-7.5 m) and its apparent size only creeps up a little (median 5-8 LEDs). The
  wall (`near * 1.15`, a hard 1/d push, card 169's Log) is doing its job of keeping a bird
  from filling the panel - which means **turning `near` down past about 3 buys very
  little apparent size** for the cost it carries.
- **That cost is real at a small count.** At `near` 6 (shipped default) and 6 birds, all
  six are in frame almost always (`in_frame` min 4-5 of 6, median 6/6/6 over three
  seeds). Turn `near` down to 1.0-2.5 with the same 6 birds and `in_frame` min drops to
  1-3, and at 3-4 birds it touches **0** on one seed - an empty panel, sometimes. A flock
  that small, seen from that close, subtends an angle the 76-degree lens does not hold:
  a modest few metres of spread is a large angle from a metre or two away. This is a
  framing problem, not the scattering problem 169 fixed - the flock's *shape* stayed
  fine (spread, clearance and the turn/speed limits never worsened) - and it is why the
  card's own hint ("closer may not be available just by lowering `near`'s minimum") was
  right, for a reason a little different from what card 169 found.
- **A lens does what the card guessed it would.** Rendered (not measured - by eye,
  `screeny-art snapshot`) a bird's *drawn* wingspan scaled up with `near` held at its
  safe, well-tested default (6.0, where the framing numbers above are excellent) and the
  bird count turned down: unmistakable V-shaped birds with a countable wingbeat, at
  `birds 6-12`, with none of the framing risk above, because the camera's simulated
  position and every limit around it are untouched - `size` never reaches `sim.rs`.

**What shipped, following the pictures and the numbers above:**

- `birds`: min 30 -> **3** (default unchanged, 55). Small counts (3, 4, 5) tested over a
  full ten simulated minutes at the shipped default `near` - see the new test.
- New parameter **`size`**: "How big the birds are drawn (a longer lens, not a closer
  camera)", 0.5-3.0, default 1.0 (unchanged pixels at the default). Scales only the
  wingspan `draw_birds` draws with (`span_m = sim::SPAN * size`); nothing in `sim.rs`
  reads it, so the flight, the seat, the framing and every limit are exactly what they
  are at `size` 1 regardless of what it is set to. This is the "narrower field of view /
  longer lens" option the card named, implemented as a direct draw-time scale rather
  than by narrowing `FOV` (which would shrink the frame's field and make the framing
  problem above *worse*, not better).
- `near`'s own range (2.0-16.0) and default (6.0) are **untouched** - the measurements
  above found little to buy by lowering it further, and what card 169 already validated
  (`near 3.0` with the full flock) remains true and available.
- `samples` ("Samples per axis", the owner: "super confusing") **removed as a control**.
  It was always anti-aliasing supersampling, never a look the owner asked for, and never
  a cost that mattered (0.14-0.98 ms/frame across its whole old range, card 177's Log,
  against a 33 ms budget) - a fixed value was the right call. Hard-coded to 3 (the old
  default) as `SUPERSAMPLE`, a named constant with a comment pointing at `Params::set`'s
  refusal of an unknown id as the reason an old saved `samples` value is simply ignored,
  never reaches a patch, and costs nothing to leave unhandled.
- Every other parameter relabelled for what it does, not what it is called internally
  (table in the final report). No id, range, step or default of any surviving parameter
  changed - `choice`/`toggle` shapes untouched, `terrain` (named in the card) included.

Added `the_smallest_flock_still_flocks`: ten simulated minutes x three seeds at `birds`
3 (the new minimum) and the shipped default `near`, asserting the physics (speed and
turn stay in their bands, nobody flies into the invisible geometry) and that the picture
does not fall apart (most of the flock stays on the panel, spread stays under 10 m). The
55-bird test's own thresholds do not transfer to a 3-bird flock (a broad
nearest-neighbour distribution needs more than 3 points to have a distribution at all),
so this is a separate, smaller set of promises sized to what a flock that size can
actually promise.

`cargo test -p screeny-art --release patches::flock::`: 5 passed, 0 failed. `cargo clippy
-p screeny-art --all-targets`: silent, no `#[allow]` added.
