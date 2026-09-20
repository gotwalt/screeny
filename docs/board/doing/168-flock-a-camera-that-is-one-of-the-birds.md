---
id: 168
title: flock - birds in slow motion, seen by a camera that is one of them
type: build
hardware: no
depends: [150, 162]
owner: worker (card 168)
branch: card/168-flock
---

## Goal

The owner, 2026-09-20: "let's design an additional patch that is a simple 3d environment
where a camera is following a flock of birds. the camera should act like a bird and use the
flocking mechanism to determine its own trajectory, and the birds should interact with
invisible geometry to keep their motions constantly evolving. the birds themselves should
be drawn with very little detail, but enough to make it clear what they are. we'll likely
run this at a pretty slow rate to have just a gentle, poetic view of birds in motion. we
might want to make it possible for the colors to rotate over time."

A new patch, `flock`. Like card 155 this is a **first version to look at and argue with**:
build the idea honestly, make what is taste a parameter, and come back with pictures and
a judgement of each.

## Context

- Write it as a patch in `crates/art/src/patches/` (card 150's vocabulary). CPU, seeded,
  stepped by `ctx.dt` on a **fixed internal timestep** so the flight is the same at any
  render rate (card 161 is about to fix the rate at 30) and a pinned snapshot is the same
  PNG every time (`screeny-art snapshot flock --seed N --at S`). Start the simulation from
  an already-formed flock, or warm it up invisibly, so the first frame is already birds
  flying and not a cloud condensing.
- **Flocking** is Reynolds' boids in 3D: separation, alignment, cohesion, each over a
  neighbourhood, with a speed band and a limited turn rate so nothing twitches. 40-150
  birds is the range worth trying: the panel is 64 x 32, so more than that is fog, and
  fewer than thirty is not a flock. O(n^2) neighbours is fine at this size.
- **The camera is a bird.** Same state, same rules, same limits - it is in the flock's
  neighbour lists and they are in its own - with only what it takes to make it a *good
  seat*: it should tend to ride at the trailing edge or the flank rather than the middle
  (a camera in the middle of a flock sees a bird's tail fill the panel), and where it
  *looks* is a smoothed blend of where it is going and where the nearby flock is, so the
  birds stay in frame as it turns with them. It banks into its turns, gently; the horizon
  rolling a few degrees is most of what will make this feel like flying. Low-pass its
  orientation: the flock may jink, the view must not.
- **Invisible geometry** is what keeps it from settling into a circle: obstacles the birds
  steer round that are never drawn - a few large spheres or soft columns, a floor and a
  ceiling, and a soft boundary that turns the flock back toward the middle of its world -
  some of them drifting slowly on their own long periods, so the space the flock moves
  through is never the same twice. An occasional slow attractor (something of interest
  over there) is fair too. Laid out from the seed. The test of this is that a ten-minute
  run never looks like a loop: measure something (the flock centroid's path, the spread of
  headings) and show it does not become periodic.
- **Drawing a bird with almost nothing.** At the far side of the flock a bird is one or
  two LEDs; near the camera it may span eight or ten. What makes two LEDs a bird is
  *motion*: a wingbeat. Draw a body point and two wing strokes whose dihedral beats - up
  is a shallow V, down is a shallow inverted V, level is a dash - with each bird on its
  own phase, beat rate tied loosely to its airspeed, and short glides (wings held level or
  slightly raised) when it is descending or coasting. Bank the wings with the bird's turn.
  Project with a proper perspective camera; anti-alias everything (a distant bird should
  fade in coverage, not pop between LEDs); size and brightness fall with distance so depth
  reads. No beaks, no tails beyond what a slight body elongation gives. Rasterise birds
  into a supersampled coverage buffer rather than testing every sample against every bird.
- **The environment is only there so motion reads.** With nothing but birds on black, the
  camera's own flight is invisible. The least that works: a sky with a vertical gradient
  and a horizon (the horizon line tilting and rising is the banking and climbing), perhaps
  a sun or moon disc or a band of far cloud as a bearing reference. Keep it quiet: the
  birds are the subject. Try **both** tonal schemes and show them: light birds on a deep
  sky (true black is this panel's strength, and it is kind to a dim room), and dark
  silhouettes on a luminous dusk sky (the classic murmuration picture, but it lights every
  LED: check the APL and what the limiter does to it). Say which reads better at 64 x 32.
- **Colour that rotates over time**: a slow hue drift in degrees a minute, as
  `clocks-dials` has `wheel`; 0 is off. The sky's two ends and the birds keep their
  relationship as the wheel turns (rotate in OKLCH at held lightness and chroma so
  nothing flashes or goes muddy); offer a hue spread between sky top and horizon.
- **The wire**: a full-frame gradient is a continuous-tone picture, which is where the
  colour budget matters (brief section 2 and 3; card 102: up to 256 colours are exact when
  the index image compresses, and spatial coherence is what compresses). A vertical
  gradient is rows of one colour - cheap - until the camera banks. Quantise the sky to a
  modest number of bands deliberately if that is what keeps frames exact, and look at
  whether banding or the lossy codec looks better; report `bytes=`, the codec and `exact`
  for level flight and for a banked turn, from the snapshot's stderr line.
- **Slow.** The owner expects to run it slowly: design the default for calm (long glides,
  wide turns, a wingbeat you can count), and make `pace` a parameter of the patch itself
  in addition to the Studio's global speed, scaling simulation time only - the wingbeat may
  want its own relation to it so slow flight does not become slow-motion flapping unless
  that is the look; try both and say.
- Card 151 (in flight) adds a `seeded` flag to the patch definition; this patch is seeded
  (the world and the flock come from the seed). The orchestrator reconciles the literal at
  merge.

## Parameters to expose (a starting set; keep it short enough to use)

`birds` (count), `pace`, `calm` (turn-rate / how tightly they wheel), `near` (how close
the camera rides), `bank` (how much the view rolls), `scheme` (light-on-dark /
dusk silhouettes), `hue`, `spread`, `wheel` (deg/min), `sky` (sky level), `terrain` (how
much invisible geometry: open sky ... busy).

## Deliverables

- The patch, registered, with a README entry and the snapshot recipes.
- Tests in the house style: determinism (same seed and time, same frame); speeds and turn
  rates stay inside their limits; no bird passes through an obstacle or leaves the world;
  the camera stays within a stated distance band of the flock and keeps some minimum
  number of birds in frame over a long run; the flight is independent of render rate; the
  non-periodicity measurement above.
- PNGs for the orchestrator to show the owner (not committed): contact strips of
  consecutive frames (wingbeats need sequences, not stills) at two distances; both tonal
  schemes; level flight and a banked turn; a minute's hue drift; a close pass.
- "Open with the owner": everything that is taste.

## Acceptance

On the panel, by the owner's eye. Before that: the orchestrator looks at the strips and
two LEDs with a wingbeat read as a bird, and the view reads as flying with them.

## Log

### 2026-09-20 - claimed

Branch `card/168-flock`, cut from `main` at 531ffed. Read the card twice, then
`docs/design/generative-art-brief.md`, `crates/art/README.md`, `patches/clocks/dials.rs`
(the `wheel` hue drift, choice parameters, palette-only animation),
`patches/metaballs.rs` (`Frame::supersample`, house style for a CPU patch) and
`patches/overland.rs` (sky, time of day, `paint()`'s "below L 0.3 is true black" rule,
`mix_hue`). Also `frame.rs`, `palette.rs`, `dither.rs`, `limiter.rs`, `pipeline.rs`,
`snapshot.rs` and `bin/screeny-art.rs` so the snapshot recipes and the `bytes=` line are
the real ones.

Plan settled before writing code:

- `patches/flock/sim.rs`: `V3`, boids, the invisible world, the camera-as-a-bird.
  Fixed internal timestep of 1/60 s, driven by an accumulator over `ctx.dt * pace`, with
  the step count taken as `floor(warped / STEP + 1e-6)` so 30 fps and 60 fps land on the
  same integer number of steps at a boundary instead of differing by one.
- `patches/flock/mod.rs`: the patch, the palette and the drawing.
- Drawing into a **two-dimensional palette**: a sky value (one scalar per pixel, from the
  view ray's elevation plus the sun's glow) x an ink level (how much bird is over it).
  Both quantised with the blue-noise ordered dither, so the frame is indexed and exact
  and the anti-aliasing survives. Birds go into a supersampled coverage buffer, drawn as
  strokes with a bounding box, not by testing every sample against every bird.
