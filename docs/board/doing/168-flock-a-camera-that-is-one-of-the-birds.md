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

### 2026-09-20 - paused by the orchestrator

Stopped on request (at most two workers on this machine). Nothing was running;
`ps` is clear.

**Where I am.** The patch is written, registered and compiles, and
`snapshot flock --seed 3 --at 12 --warmup 12` renders. Its first stderr line,
the only measurement so far:

```
patch=flock seed=3 t=12.00  colours=44 bytes=1295/1464 (pal8-lz, exact)  apl=13% (patch 13%)
```

So the two-dimensional palette works as intended: **an exact frame** on the
`PAL8_LZ` rung at 1295 of 1464 bytes, 44 distinct colours of the 84 the palette
offers, 13% APL in the light-on-dark scheme (well under the limiter's 40% cap,
which is left idle at x1.00). That is level flight, default parameters, before
any tuning.

**I have not looked at a single picture yet.** Nothing below is judged: the
shapes, the wingbeat, the seat in the flock, the band count, the horizon step,
the dither choice and every default are all still first guesses.

**Next step when resumed**, in order:

1. Read `first.png`, then render sequences (`--at` stepped by 1/30 s) and stack
   them into contact strips with PIL; judge whether two LEDs with a wingbeat
   read as a bird and whether the view reads as flying *with* the flock.
2. Instrument the long run: birds in frame (min/median), nearest bird's size,
   view yaw and roll rates (deg/s, p95), clearance from the blobs,
   non-periodicity by autocorrelation of the centroid path and heading.
3. Both tonal schemes, level flight and a banked turn, with `bytes=`, codec,
   `exact` and APL for each.
4. The rest of the test suite, README entry, clippy, `cargo test --release`.

**Things learned that are not yet written down elsewhere:**

- The sky is one ramp that is **brightest at the horizon** and falls off both
  upwards and downwards, with a small step at the horizon so it is a line and
  not just the top of a gradient. That was not the first plan: a zenith->horizon
  ramp cannot also carry the ground, and a separate ground ramp doubles the
  palette. One ramp is also what a hazy sky at altitude really looks like.
- The sun has to sit **near the horizon**. The sun is the top two entries of
  that same ramp, and the glow blends the band coordinate from sky to sun; if
  the sun were high, that blend would sweep through every intermediate band and
  draw a rainbow ring round it. Near the horizon the sky is already at the top
  of the ramp, so the sweep is two bands long and reads as a halo.
- `terrain` must not re-roll the world. All five blobs are laid out from the
  seed at birth and `terrain` only says how many of them are real this frame,
  so the slider can be moved live without the world changing under the flock.
- Rate-independence is bought with one line: the step count is
  `floor(simulated_seconds / STEP + 1e-6)` rather than a drained accumulator, so
  30 fps and 60 fps agree on the integer at a step boundary even though their
  float sums differ in the last bits. Untested as yet.
- Avoidance is a push **across** the flight path, not back along it, with a
  reach of `6 + 1.1 * vmax / turn` metres, so it scales with how tightly the
  flock is allowed to turn. Whether that is enough to keep every bird outside
  every blob is exactly what the long run has to show.

### 2026-09-20 - resumed: the flight, measured into shape

Merged `main` (card 151's `seeded` field; `flock` says `seeded: true` - the world, the
sun and the flock all come from the seed). Then built the long-run measurement **before**
tuning anything, which is the only reason any of the below was found rather than guessed
at. One ten-minute run at the defaults answers every question the card asks; each finding
below is a number that failed and the change that fixed it.

1. **The flock never moved.** `fly()` integrated velocity and never position. `first.png`
   was 81 birds hanging in the air beating their wings. Found in thirty seconds by
   printing the centroid every fifteen seconds and watching it not change.
2. **The camera was 1.5 m from the nearest bird** - a 25-LED wing across the panel. Its
   personal space is now `near * 1.15` and its separation weight is 7.0 against a bird's
   3.4. `near` now means what it says.
3. **The world was too small for the speed.** The flock crossed a 60 m world in 25 s and
   lived at the boundary. `BOUND` 100 m, floor/ceiling -24..28 with a 13 m margin and a
   standing spring to the cruise band, blobs scaled with it.
4. **The camera must be *more* agile than a bird, not less.** A camera whose turn radius
   is larger than the flock's circle is thrown off it every time they wheel; it then
   watches dots for half a minute. Turn budget 1.15x a bird's, and the smoothness moved
   entirely into where it *looks*.
5. **It also has to be able to fly slower.** Once ahead of the flock it could not drop
   back, because its minimum speed was the flock's. Its band is now 0.5x to 1.3x.
6. **The view snapped through 180 degrees.** A lerp between directions passes through a
   short vector whose direction is rounding noise. `turn_towards` walks the arc instead
   and carries a hard ceiling.
7. **The look needs a leash, on the look and not on its target.** Manoeuvring into its
   seat the camera's heading is up to 70 degrees off the flock; a 55% blend of that is
   still outside a 38-degree half-field. The flock's middle is now never more than 16
   degrees off the view axis. Leashing the *target* was not enough: a low pass 50 degrees
   behind its target still shows an empty panel.
8. **The aim point was a mean of unit vectors**, which falls apart when birds are spread
   around you - the horizontal parts cancel and the bearing spins. The long run caught it
   as a two-second burst at 80 deg/s with the flock sitting still. It is now the direction
   to the weighted mean *position*.
9. **`level()` must be applied last, and to the aim point too.** Applied before the leash
   it was simply undone and the horizon sat off the top of the frame.
10. **The camera rides *below* the flock** (`near * 0.45`), so the view is a little
    nose-up and the horizon sits low with the birds against the sky. Level or above, the
    view spends its life pinned at the bottom of its band with two thirds of the panel
    dark ground - the picture upside down.
11. **Trail along the flock's ground track, not its course.** Following a diving flock
    down its own vector parks the camera above it, and a view that must hold the horizon
    then has the flock below its feet.
12. **Nothing climbs or dives more steeply than 25 degrees.** This was the last source of
    empty frames, and it is also most of what makes the flight read as cruising rather
    than aerobatics.
13. **`calm` now means a turn radius.** Remapped to 1.10 - 0.95*calm rad/s (a 4.5 m wheel
    at 0, a 33 m one at 1), default raised to 0.85. This matters more than anything else
    because *the view must pan at the flock's own turn rate* - that is geometry, not
    taste - so a flock that wheels at 48 deg/s cannot be watched calmly from inside it.
14. Separation 2.6 -> 3.6 m and birds 80 -> 55: at 80 tightly packed the panel was fog,
    which is exactly what the brief warns about. Haze strengthened a lot (16 m halves a
    bird's contrast), because at this size **depth reads as contrast far more than size**.

Ten minutes at the defaults, seed 11, 55 birds, all inside their limits:

```
in frame  min 30  median 49  p05 42
nearest   min 3.6 m  median 7.4 m   biggest bird median 5.4 LEDs, p95 6.8
seat      median 13.5 m from the centroid, p95 16.7 m
view      yaw p95 15.5 deg/s (max 25.8), roll p95 2.14 deg/s (max 4.58)
limits    speed x1.000, turn rate x1.024, blob clearance 9.7 m
loop      strongest self-similarity r=0.52 at 146 s
```

The periodicity measurement was wrong at first and flattered itself to r=1.00 on a path
that never repeated: it normalised against the whole series' energy, and a smooth wander
correlates with itself at every lag. It is now a Pearson correlation of the two
overlapping windows, run over the **stationary** series (how fast the flock drifts and
how fast it turns) rather than over where it happens to be.

Frames are exact: `colours=43 bytes=1218/1464 (pal8-lz, exact) apl=8%`, limiter idle.

### 2026-09-20 - the picture

Contact strips, read at 8-10x. What was found by looking, in order:

- **The sky came out upside down** - horizon in the top quarter, two thirds of the
  panel dark ground. Diagnosed by printing the palette index down the centre column
  rather than squinting: `level()` (hold the horizon inside a band) was being applied
  *before* the leash, which then undid it. Applied last, and with the camera seated a
  little **below** the flock so the view rides nose-up, the horizon sits in the lower
  middle with the birds against the sky.
- **Eighty birds was fog**, exactly as the brief warns. 55, a looser flock
  (separation 3.6 m) and a much stronger haze. At 64x32 **depth reads as contrast far
  more than as size**: 16 m of air halving a bird's contrast is what separates the
  near birds from the far ones, where making them smaller does almost nothing.
- **The dusk scheme was muddy.** The bird had its own hue (the complement), so every
  half-covered pixel between a pale sky and a dark bird landed on a near-grey
  in-between - the one thing brief 2.2 says this panel cannot show. A silhouette is
  the sky *darkened*: same hue, much lower lightness. Its sky also needed a far wider
  lightness range (all one bright lavender has nowhere for a silhouette to sit, and
  lights every LED: APL 39% -> 20%), and its haze needs a much higher floor, because a
  hazed silhouette on a lit sky is a smudge where a dim white bird on black still reads.
- **Sky dither**: tried blue noise, Bayer 4x4, Bayer 8x8 and none, in both schemes and
  on a rolling horizon. They are very hard to tell apart - twelve bands over the
  vertical field is already fine enough that undithered does not obviously band - and
  they cost 1197 / 993 / 1006 / 846 bytes. Bayer 4x4, because the brief is right that
  an ordered pattern compresses where a random one does not, and because the headroom
  measurement (worst 1194 of 1464) says the 150 bytes are affordable. Blue noise is
  kept for the ink, where the areas are small.
- `near` **0.35 s** of the panel: at 2.5 m the birds are 8-10 LEDs and unmistakable;
  at 14 m they are a distant shoal. The default stays at 6.0 for a reason that is not
  taste - see "Open with the owner".

Rejected along the way: a bright zenith falling to a dark horizon (cannot also carry
the ground without a second ramp, and doubles the palette); a high sun (the glow
blends the band coordinate from sky to sun, and from a *dark* band that sweep passes
through every intermediate colour and draws a rainbow ring - low, where the sky is
already at the top of the ramp, the sweep is two bands long and reads as a halo); a
firm repulsive "wall" at a blob's surface (entirely wasted - the cruise turn-rate
limit clips it to about 2 m/s^2 - which is what led to giving a dodging bird more turn
rate instead); blending the view only 55% towards the flock (leaves half the panel
empty sky; 68% with a gentler flank offset is better).

### 2026-09-20 - wingbeat and pace

`pace` scales simulation time only, and the wingbeat rides on simulation time, so at
`pace` 0.3 the birds flap in slow motion - which is a real look, and not the only one
wanted. `beat` is therefore its own control in Hz: at `pace 0.3, beat 2.4` the wings
move dreamily, and at `pace 0.3, beat 5.0` the flock drifts slowly while the wings
work at a believable rate. The second is more convincingly *birds*; the first is more
convincingly a dream. Both strips are in the scratchpad; this is the owner's call.

### 2026-09-20 - the numbers, tests and clippy

`cargo clippy --workspace --all-targets`: **silent**. Four fixes were needed in this
patch's own code (a dead `elevation`, three test-only helpers now `#[cfg(test)]`, and a
`needless_range_loop`); no `#[allow]` was added anywhere.

Four tests, all in `patches/flock/tests.rs`:

- `ten_minutes_of_flight` - one ten-minute run per seed over **three seeds**, asked
  every question at once: birds in frame, the camera's seat and the nearest bird,
  speed and turn-rate limits, clearance from the invisible geometry (flock and camera
  separately), the view's yaw and roll rates, and non-periodicity.
- `every_frame_goes_out_exactly` - 1800 frames of each scheme through the real encoder
  and decoder.
- `the_flight_does_not_depend_on_the_frame_rate` - a minute at 30 and at 60 fps.
- `the_same_seed_at_the_same_moment_is_the_same_frame`.

Follow-up written as card 169.
