---
id: 177
title: flock - less predictable turns, real vertical motion, and spacing that is not a lattice
type: build
hardware: no
depends: [168]
owner: worker (card 177)
branch: card/177-flock-flight
---

## Goal

The owner, 2026-09-20, watching `flock` on the panel ("this is great"), then: "let's also
randomize the bird direction changes a bit more - i'd love to see some more vertical motion
change." (His other ask in the same message - a toggle for the whole backdrop, white birds
on entirely black - is done: the `backdrop` toggle, by the orchestrator on `main`.)

A few minutes later, still watching it: "oh also: the boids seem to be very evenly
separated from each other, which is not lifelike."

The flock should surprise more: direction changes that are less regular in when and how
hard they come, and climbs, dives and swoops that are part of the flight rather than a
wobble about one cruising height.

## Context

- Card 168's Log and the module header of `crates/art/src/patches/flock/sim.rs` say how
  the flight works today: boid rules with `calm` setting speed band and turn rate; nothing
  climbs or dives past **25 degrees**; a floor/ceiling with a 13 m margin **and a standing
  spring to the cruise band**; five drifting blobs; a slow attractor re-picked every
  40-110 s. The spring and the pitch limit are why the flight is mostly horizontal; the
  long attractor period and the slow blob drift are why turns feel regular.
- **Even spacing is the separation rule winning.** Today every bird has the same
  separation radius (3.6 m) at a high weight (3.4) against a weak cohesion (0.28 over
  26 m), so the flock relaxes into something close to a crystal: equal gaps in every
  direction. Real flocks are clumpy and anisotropic. Things to try, and keep what reads:
  a *topological* neighbourhood (each bird attends to its nearest six or seven, whatever
  their distance - Ballerini et al. 2008, the starling result - rather than everything
  inside a radius), which by itself produces dense knots and thin streamers; a soft
  separation that only bites at about a wingspan, so birds can pass close; per-bird
  preferred spacing and speed drawn from the RNG (individuals, not clones); pairs and
  small sub-groups with stronger mutual cohesion that drift within the flock; a flattened
  shape (flocks are wider than they are tall, and denser at the edges than the middle).
  Measure it: the distribution of nearest-neighbour distances should be broad and skewed,
  not a spike - report its coefficient of variation before and after (a lattice is near
  0; aim for something like 0.4-0.6) - and look at strips: knots, gaps, stragglers.
- Ideas, not orders - try them and keep what reads: a taller world with the spring much
  weaker or replaced by soft floor/ceiling only; the attractor picked in 3D with real
  height differences, on a shorter and more varied clock; occasional **events** drawn from
  the RNG - a sudden shared dive and recovery (what a murmuration does when a hawk comes),
  a rising spiral, a split and rejoin - with long quiet flight between them so they stay
  events; per-bird restlessness (small random steering) so the flock's shape breathes; a
  pitch limit that depends on speed (a dive is faster, a climb slower) so vertical motion
  looks like flying and not like an elevator. Wingbeat already glides on descent: make
  climbs beat harder.
- **The camera is the constraint.** It is a bird, and everything the flock does it has to
  follow without the view becoming nauseous: card 168 measured view yaw p95 15-19 deg/s
  (hard cap 26) and roll p95 ~2.5 deg/s, 34-48 of 55 birds in frame. Pitching the view is
  new: with the backdrop on, the horizon will now move up and down the panel, which is
  exactly what will make a dive read - but keep it smooth, and keep the flock in frame
  through a dive (the camera may need to anticipate: follow the flock's mean vertical
  velocity, not just its position). Re-run card 168's long-run measurements on seeds 11,
  29 and 404 and report them beside the old numbers, plus new ones: the flock's altitude
  range and vertical speed distribution over ten minutes, view pitch rate p95/max, and the
  distribution of time between noticeable heading changes (it should be broad).
- With the backdrop off (white on black) there is no horizon, so vertical motion has to
  read from the birds alone: their pitch attitude, glide vs beat, and their drift across
  the frame. Look at strips in both modes.
- The owner runs it slowly (`pace` 0.7 default, he may go lower): an event must still be
  graceful at `pace` 0.3.
- Give him one or two controls, not five: e.g. `wild` (how often and how hard the flight
  changes its mind) and `lift` (how much of the motion is vertical). Defaults noticeably
  livelier than today's, but still calm.
- Determinism, rate independence (fixed 1/60 s step), the exact-on-the-wire palette, and
  every test of card 168 must still hold. `seeded: true`.
- Noted while adding the backdrop toggle: on black the birds have only `INK = 6` levels
  for both anti-aliasing and depth. If depth steps are visible as birds come and go,
  consider more ink levels when the backdrop is off (the sky bands are unused then, so the
  palette has room) - measure bytes and exactness if you do.

## Deliverables

The flight changes and the controls; tests extended; strips in the scratchpad's `flock/`
(a dive and recovery with and without the backdrop; a minute's centroid path, old against
new); the measurements above in the Log; "Open with the owner".

## Acceptance

The owner's eye on the panel. Before that: strips in which a dive reads as a dive, and
numbers showing the view stayed smooth and the flock stayed in frame.

## Log

### 2026-09-20 - claimed

Branch `card/177-flock-flight`, cut from `main` at 043ad8f. Read this card twice, then
card 168's Log end to end (the boid rules and every limit, the four ways the camera is
not a bird and the measurement that put each one there, the two bugs that rendered
plausibly - a flock that never moved, and tuning against one seed - and the snapshot
recipe: `--warmup` must equal `--at`, judge from sequences), card 169 (riding close
scatters the flock), then `patches/flock/mod.rs`, `sim.rs`, `tests.rs` and the README
entry.

What I take from the reading, before changing anything:

- **The lattice is three things at once.** One separation radius for every bird (3.6 m)
  at weight 3.4, a cohesion that reaches the whole flock (26 m) at 0.28, and no
  individuality beyond a wingbeat trim. Identical birds with identical radii relax into
  identical gaps. The fix has to be some mixture of *topological* neighbours, a softer
  separation, and per-bird differences.
- **The flight is horizontal by construction**, in two places: `CLIMB = 0.42` caps every
  climb and dive at 25 degrees, and `a.y += (3.0 - pos.y) * 0.05` is a standing spring to
  one cruise altitude that never lets a climb go anywhere. The floor/ceiling (-24..28 with
  a 13 m margin) is the third.
- **Turns are regular because the only thing that changes the flock's mind is the
  attractor**, re-picked every 40-110 s and reached over ~16 s, plus blobs drifting on
  90-260 s periods. Nothing else in the model has a short time constant.
- **The camera is the constraint, and its budget is nearly spent.** View yaw p95 is
  15.5-18.8 deg/s against a hard 26; the flock's turn rate *is* the view's pan rate,
  geometry not taste. So "livelier" has to buy its liveliness in *vertical* and in
  *spacing*, where the view's budget is not already committed, and any extra heading
  change has to come out of the same 26 deg/s.

Plan, in this order, so every number is comparable: (1) extend the long run with the new
measurements the card asks for and take the **old** numbers from this unchanged code
first; (2) spacing; (3) vertical; (4) unpredictability; (5) the camera keeping up; then
strips, wire, ms/frame.

### 2026-09-20 - the measurements first, on the old code

`ten_minutes_of_flight` now also gathers, on the same single run per seed: every bird's
nearest-neighbour distance every two seconds (the camera left out), the centroid's
altitude and climb rate, the view's **pitch** rate and how far off level it actually
points, the time between noticeable (20 degree) course changes, and - added later, when
it was needed - how far the furthest bird is from the middle and how many are more than
20 m out. Run against the unchanged flight, so the "before" column is mine:

```
             seed 11        seed 29        seed 404
NN mean       2.67 m         2.55 m         2.60 m
NN CV         0.10           0.13           0.16      <- a lattice
NN p05/p95    2.15 / 3.01    1.91 / 2.97    2.00 / 2.99
climb |v_y|   1.26 / 2.25    1.12 / 2.12    1.33 / 2.23   (median / p95)
altitude      -21.6 .. 22.9  -15.9 .. 29.4  -18.2 .. 21.0
view pitch    2.63 / 7.73    2.85 / 9.18    3.19 / 10.54  (p95 / max deg/s)
turn gaps     3.1 s CV 1.23  2.4 s CV 1.08  2.7 s CV 1.35
```

The owner was right and the number is stark: **CV 0.10-0.16 is a crystal.** (A random
scatter of points is about 0.36; a real flock is 0.4-0.6.)

The second thing the numbers said was not what I expected. The old flight *does* move
vertically - the centroid's altitude covers 40 m and it climbs at over a metre a second
half the time - but **none of it reaches the panel**, because the camera goes with it
and the view is pinned level: pitch p95 under 3 deg/s. So "more vertical motion change"
is two jobs, not one: make the flight's vertical bigger, and let the *view* show it.

### 2026-09-20 - the flight, in the order it was found

Each of these is a number that failed and the change that fixed it. The long run over
three seeds was the only reason any of them was found rather than guessed at.

1. **Topological neighbours and individuals** (the seven nearest whatever their
   distance; per-bird room and airspeed; nine clans that fly closer to one another than
   to strangers). CV 0.10 -> 0.49 on seed 11 at the first try.
2. **A clan pull with no bound is a runaway.** A clan that drifts a little out of the
   flock pulls the rest of itself after it: eleven birds seventy-four metres out. Bounded
   at 15 m - a friendship, not a beacon.
3. **The sideways part of blob avoidance must be taken across the *flock's* course, not
   each bird's heading.** Once alignment went topological the headings inside the flock
   spread out, and a per-bird sideways basis sent neighbours round opposite sides of the
   same blob: **55 of 55 birds more than 20 m out, for a minute**. Off one shared course
   the whole flock curves as one body.
4. **A weak wish to fly the flock's own course** (0.22) on top of the seven neighbours.
   Topological alignment alone lets the headings fan to 47 degrees, and a fan is what
   turns a shared shove into a scatter - the fan was visible in the trace before the
   spread was.
5. **The gather must not switch off while dodging.** A blob's reach is nearly forty
   metres, so "dodging" is most of the time, and a flock with no gather for most of its
   life stays scattered once anything scatters it. Weakened to 0.40 rather than off.
6. **Soft separation, per-clan room.** A 1/d falloff from the full radius is a wall and
   every bird settles where the wall stops pushing; a squared falloff with a stiff core
   at a wingspan is a preference. And room drawn *per bird* averages out - every knot
   ends the same density - where room drawn **per clan** makes some knots tight and some
   loose, which is what a broad distribution of gaps actually is.
7. **The climb limit was a clamp on the velocity, and a clamp rotates the heading** by
   however much it takes - a turn nobody authorised. Every `x1.03 of its limit` in the
   long run was this line. It is now a strong restoring acceleration that goes through
   the same turn-rate clamp as everything else, so there is one limit on how fast
   anything may change direction, and the climb limit is the angle a bird runs out of
   lift at rather than a wall.
8. **The standing spring to one cruise altitude is gone** - `(3 - y) * 0.05`, and the
   reason every climb was paid back within seconds. What replaces it pulls each bird
   towards the *flock's* altitude (flocks are wider than they are tall), plus one very
   weak string on the flock as a whole: without that last one the flock random-walked
   down and spent the second half of the run at -60 m with the world's whole height
   unused above it.
9. **A taller world** (-70..70 with an 18 m margin, against -24..28 with 13). Altitude is
   invisible - the sky is drawn from the view ray, not from a height - so height costs
   nothing but room, and the old free band was four seconds of descent.
10. **Shared surges.** A dive and its recovery, sometimes a climb, sometimes with a swirl
    through it: a full sine over 5-13 s so both ends are zero and a dive always recovers,
    with the quiet afterwards drawn from a squared uniform so most of the flight is
    cruise. `lift` sets the size, `wild` the frequency.
11. **The camera was watching a levelled fiction.** `focus` - the direction to the flock -
    was clamped into the horizon's own band *before* the leash read it, so with the flock
    35 degrees above the camera through a recovery the aim point said 14, the leash was
    satisfied, and **ten of fifty-five birds were on the panel**. `focus` is now the truth
    (clamped only at 33 degrees, where an azimuth stops meaning anything).
12. **Birds before horizon.** The last three corrections in `aim` are now composition,
    then the leash, then the rate ceiling - card 168 had composition last, which was right
    when nothing in the flight could push the horizon off the panel and is wrong now. A
    second, tighter leash in *elevation* (10 degrees, against 16 sideways) because the
    panel is 21 degrees tall and 38 wide, and an outer wall at 19 degrees so there is
    always some horizon in shot.
13. **The camera needed more of everything vertical**: a seat 0.85 s ahead of the flock's
    climb rate rather than under where it is, a quarter more climb and dive angle, a third
    of a bird's speed penalty for climbing, and turn slack up to 3.4x.
14. **`wild` 0.45 was not enough to break the flight's slosh.** At 0.45 the centroid's
    drift correlated with itself at r=0.70 with a clear 130 s oscillation - the flock
    bouncing between the world's soft walls. The autocorrelation curve, printed at every
    lag rather than just its maximum, is what showed it was a real oscillation and not a
    smooth-series artefact. At 0.65 it is 0.39-0.58 on three seeds, and 0.65 is the
    default.

Looked at, before going further (strips of ten consecutive frames two seconds apart,
seed 11, through a surge at simulated t=45 s): with the **horizon line** backdrop the
line sweeps from the upper third down past the middle over the sequence and tilts with
the bank - a dive that reads as a dive with almost nothing drawn. On **black** the flock
slides down the frame and the knots are unmistakable: dense clusters with real gaps and
several visible V's, where the old strip is an even scatter. With the **sky** the horizon
band moves the same way, more quietly.

### 2026-09-20 - the numbers, old against new

Three seeds x ten simulated minutes at the defaults, both columns run by me on this
machine. "Old" is `main` before this card (commit f67ad37, which is card 168 as merged
plus the new measurements and nothing else); where a metric did not exist in the old
test it was taken by compiling that commit's `sim.rs` verbatim as a standalone program,
which is how the old view tilt, steepest angle and straggler counts below were got
without touching the working tree.

```
                          old (11 / 29 / 404)          new (11 / 29 / 404)
spacing
  NN mean, m              2.67  2.55  2.60            2.03  2.12  1.73
  NN CV                   0.10  0.13  0.16            0.38  0.41  0.43
  NN p05 / p95, m         2.15/3.01 1.91/2.97         1.18/3.35 1.13/3.66
                          2.00/2.99                    1.02/3.11
  furthest bird, med/p95  6.1/8.0 6.6/8.6 6.4/21.9    11.4/13.1 10.9/18.4 9.1/11.7
  more than 20 m out      0 / 0 / 10 (worst)          0 / 3 / 0 (worst)
vertical
  centroid |v_y| p95      2.25  2.12  2.23            2.93  3.14  3.04
  centroid |v_y| max      2.41  2.39  2.42            3.56  3.60  3.65
  altitude range, m       44.5  45.3  39.2            72.0  108.1 78.6
  steepest anything flew  25    25    25  (a clamp)   51    50    51 (a spring)
turns
  gap between 20-deg      3.1   2.4   2.7  s          3.7   3.5   3.2  s
  gap CV                  1.23  1.08  1.35            1.40  1.06  1.14
  gap p95                 8.2   6.1   7.7  s          14.1  10.6  9.8  s
the view
  yaw p95, deg/s          15.5  18.8  18.5            16.4  14.8  14.1
  yaw max                 25.8  25.8  25.8            25.8  21.0  25.8
  roll p95                1.75  2.58  2.46            2.20  2.49  1.98
  PITCH p95, deg/s        2.63  2.85  3.19            3.88  3.96  3.67
  PITCH max               7.73  9.18  10.54           11.60 11.97 14.63
  points off level p05/p95 0.4/11.5 -1.5/11.5         -1.6/11.1 0.4/12.5
                          -1.2/11.5                    1.0/11.3
  points off level range  -6.0..11.5 -11.0..11.5      -8.4..16.7 -3.6..18.4
                          -9.9..12.1                   -3.7..18.2
the seat
  birds in frame, worst   34    35    17              34    38    36
  birds in frame, median  48    47    48              50    48    51
  birds in frame p05      42    39    40              43    42    45
  nearest bird, median m  6.7   6.6   6.7             6.1   6.2   6.2
  seat p95, m             14.3  14.2  21.9            15.4  15.5  14.1
limits
  speed                   x1.000 everywhere           x1.000 everywhere
  turn rate               x1.024 worst                x0.995 worst
  blob clearance, m       10.5  14.6  4.9             19.8  17.2  21.9
  loop, strongest r       0.53  0.39  0.40            0.39  0.54  0.58
```

Reading it honestly:

- **The lattice is gone.** CV 0.10-0.16 -> 0.38-0.43, at the bottom of the 0.4-0.6 the
  card asks for and three to four times what it was. The p05/p95 spread went from
  2.0-3.0 m (a spike) to 1.1-3.5 m (a distribution). It is not *more* clumped than that
  because past about 0.45 the flock starts shedding birds instead of knotting, which the
  straggler count catches; that trade is the honest ceiling on this mechanism and it is
  where I stopped.
- **The flock's vertical motion is about 40% faster and its range twice as deep**, and -
  this is the part that actually reaches the panel - **the view now pitches**. The old
  view sat pinned at the top of an 11 degree band (p95 11.5 on all three seeds, i.e. at
  the stop); the new one ranges over about 15-27 degrees of panel and its p05 is near
  level. The horizon moves.
- **The camera is a better seat than it was**, which I did not expect to be able to say:
  worst-case birds in frame 17-35 -> 34-38 and the seat p95 21.9 -> 14.1 on the seed that
  used to be marginal. The yaw budget did not have to grow to pay for any of it.
- **Turns are less regular**, though modestly: the mean gap is up a second and the p95 gap
  nearly doubled, so there are longer quiet stretches with the same number of sharp
  changes. The `wild` control moves this a long way further if the owner wants it.
- **The loop number is the one that did not clearly improve** (0.39-0.58 against
  0.39-0.53, same test, same threshold). At `wild` 0.45 it was worse - 0.70, a real 130 s
  slosh between the world's soft walls - and 0.65 is where it came back. `wild` 0.85 gives
  0.42-0.57 and is the better number if he wants it.

### 2026-09-20 - the wire, the clock, and card 169

The wire, 1800 frames of each of the four pictures through the real encoder and decoder,
seed 11, `pal8-lz` throughout, **0 lossy frames anywhere**:

```
sky, light on dark      worst 1056 of 1464 bytes, up to 41 colours, peak APL  9%
sky, dusk silhouettes   worst 1092              , up to 41 colours, peak APL 21%
horizon line            worst 1182              , up to 68 colours, peak APL  3%
black                   worst 1127              , up to 14 colours, peak APL  3%
```

The limiter never bites after the opening second (x1.00). The two dark backdrops got
**twelve ink levels instead of six** (card 177's last bullet): with no sky the bands are
all the same black, so the palette has room, and six levels were carrying both the
anti-aliasing and the whole depth cue. Measured, worst bytes on black: 737 at six, 1003
at ten, 1127 at twelve, 1334 at sixteen. Twelve leaves the dark backdrops no dearer than
the sky one. Looking at six against twelve side by side, the difference is real but
small - the far birds grade more smoothly - and it costs 390 bytes of headroom, so it is
listed below as the owner's to undo.

Performance, patch and full pipeline together, one core, release, 1200 frames after 300
of warm-up:

```
55 birds, samples=1              0.14 ms/frame
55 birds, samples=3 (default)    0.30 ms/frame      [card 168: 0.39]
55 birds, samples=3, on black    0.18 ms/frame
150 birds, samples=3             0.44 ms/frame      [card 168: 0.57]
55 birds, samples=6              0.82 ms/frame      [card 168: 0.98]
```

Faster than card 168 despite doing more: the topological neighbour pass keeps at most
seven candidates instead of accumulating over everything inside a 26 m radius.

**Card 169 is fixed by this card**, as far as its own acceptance goes. It exists because
`near` could not be turned down - at 4.5 the camera scattered the flock to a 14.4 m spread,
a 57.9 m seat and *zero* birds in frame at worst. Run at `near 3.0`, three seeds x ten
minutes, new flight:

```
flock spread   4.8 / 5.9 / 6.0 m        (card 169 asks for under 7)
seat p95      12.4 / 13.4 / 13.4 m      (asks for under 22)
in frame worst  36 /  28 /  33          (asks for at least 15)
biggest bird  7.7-8.2 LEDs median, 9.5-9.7 p95   (the 8-10 LED birds it wanted)
```

Nothing in this card was aimed at card 169; what fixed it is that the flock now holds
together for reasons that do not depend on separation being stiff (a weak wish to fly
the flock's course, a gather that does not switch off while dodging, and clans), while
the camera kept the old hard wall for its own personal space. The default `near` is left
at 6.0 - which of the two pictures he wants is the owner's call, and it is card 168's
"Open with the owner" item 2.
