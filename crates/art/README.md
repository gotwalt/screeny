# art: generative art for the screeny panel

Design brief: [`docs/design/generative-art-brief.md`](../../docs/design/generative-art-brief.md).
Read it first; this code is that brief turned into a pipeline.

Two crates in the repo's single workspace (`crates/`; host toolchain, not the `esp` one):

| | |
|---|---|
| `crates/art` (`screeny-art`) | Library + headless binary. Patches, panel model, dither, limiter, statistics, outputs. No GUI dependencies; this is what will run on a server. |
| `crates/studio` (`screeny-studio`) | The Studio: an axum server with a browser page, attached to one panel. The page is a window onto what that panel is playing, drawn as LEDs, and where patches are designed. |

Nothing here opens the serial port or implements the wire protocol - that is
`crates/proto` and `crates/screeny`, and this crate calls them. Frames leave
through the `Output` trait (`crates/art/src/output/`), of which
[`SenderOutput`](src/output/sender.rs) is the real one.

## Run

```bash
# from the repo root
cargo run -p screeny-studio                      # the designer
cargo run -p screeny-art -- list                 # patches and their parameters
cargo run -p screeny-art -- pipe plasma | ...    # raw 6144-byte sRGB frames on stdout, 30 fps
cargo run -p screeny-art -- snapshot plasma --seed 7 --at 6 --out plasma.png
cargo run -p screeny-art -- snapshot clocks-numerals --time 21:12 --out clock.png
cargo test -p screeny-art                        # includes the end-to-end wire tests

# to a panel (use --release, the encoder is ~10x slower in a debug build)
cargo run --release -p screeny-art -- play plasma --to screeny-4a00a4

# the network-free build: no sockets, no mdns-sd, no `play`
cargo build -p screeny-art --no-default-features            # also no GPU patches
cargo build -p screeny-art --no-default-features --features gpu
```

## Sending to a panel

`play <patch> --to NAME|ADDR` streams a patch over UDP through
[`screeny`](../screeny)'s `Link`. `--to` takes three shapes, told apart by
`Target::parse` (card 146):

| you write | it is | found by |
|---|---|---|
| `screeny-4a00a4` | an mDNS instance name - **preferred**, because the link re-resolves it on every reconnect and so follows the device across a DHCP lease | a browse |
| `10.0.0.5`, `10.0.0.5:49374` | an address | nothing; used as given |
| `host.docker.internal:49374`, `panel.lan` | a host name: anything with a dot or a port in it | the system resolver, on the link's connect thread, again on every reconnect |

`--wait` starts without a panel and picks one up when it appears; `--seconds`
bounds the run; ctrl-c sends `FINAL` so the panel is released at once.

Three things are worth knowing before building on it:

- **Indexed frames go on the wire exactly.** Up to 32 colours whatever the
  indices, up to 256 when they compress. Pixel-exactness is checked end to end
  against `screeny-sim` in [`tests/sender.rs`](tests/sender.rs).
- **One rate: 30 fps** (`screeny_art::FPS`, card 161). It is the panel's rate,
  it is what `play`, `pipe` and the studio's players render at, and there is no
  flag or control that offers another. The link still applies the device's
  cadence ceiling itself - it never sleeps and never bursts - but a patch handed
  to it at 30 has nothing to fold away, so `Coalesced` sits at zero instead of
  taking half the frames.
- **The network cannot fail a send.** A panel that reboots, moves or is off is a
  run of counters in `PanelStatus`, not an error in the render loop.

### The `sender` feature is on by default

Decided by the orchestrator on 2026-09-19 (card 112). `SenderOutput` and `play`
live behind the `sender` feature, and that feature is **default-on**, alongside
`gpu`:

- The art system is the project's primary sender, so a build of it that cannot
  send is the special case, not the other way round.
- [`tests/sender.rs`](tests/sender.rs) is `#![cfg(feature = "sender")]`. It is
  the only place the pixel-exactness claim is checked *on the wire*, against
  `screeny-sim`; off by default it ran zero tests in a plain `cargo test` and a
  regression in the link, the chooser or the pipeline would have passed CI.
- `gpu` is already default-on with `--no-default-features` as the escape, so a
  second feature behaving differently was a trap. It had already caught someone:
  a plain `cargo test --release` at the root rebuilt `target/release/screeny-art`
  without `play`.

**The network-free build is `cargo build -p screeny-art --no-default-features`**
(add `--features gpu` to keep the GPU patches). It has no sockets, no mdns-sd and
no ctrlc, the core - patches, pipeline, meter, preview - is all there, and
`screeny-art play` is gone from the binary with a usage line that says why.
`cargo test -p screeny-art --no-default-features` is green; `tests/sender.rs`
compiles to nothing there, which is the point of the `cfg`.

The **meter** is not behind a feature: `screeny-encode` and `screeny-proto` open
no sockets, so the studio's frame statistics and preview are the real encoder's
answers whether or not a panel is anywhere nearby.

The studio needs no Node toolchain and no `tauri-cli`: the front end is three
static files in `crates/studio/ui/`, embedded at build time. Edit them and re-run.

Studio keys: `Space` pause, `R` restart, `N` new seed, `1` `2` `3` LEDs / squint / pixels.

## Pipeline

```text
patch -> limiter -> quantise to the panel's duty steps (ordered dither) -> WireFrame -> outputs
                                                             \-> meter: real encode
                                                                      -> real decode -> preview
```

- A **patch** turns (wall-clock `t`, seed, parameters) into a `Frame`: either
  linear-light RGB, or a palette of up to 256 colours plus indices. Indexed
  frames pass through exactly - 32 colours whatever the indices do, more when
  the index image compresses (see the assumptions section). Prefer them.
- The **limiter** caps average picture level and the rate at which mean
  luminance (and mean red) may rise, so no patch can strobe the panel.
- The **meter** (`meter.rs`) runs the sender's own chooser and the firmware's
  own decoder over every frame, so the codec, the byte count, the exactness
  decision and the preview picture are measured rather than estimated. The
  preview is literally the decoded datagram.
- The studio's four meters are the four numbers from brief section 5.

## Telling a run what time it is (`--time`)

Patches that tell the time read `ctx.now` (local time of day), never the system
clock. Where that comes from is `patch::Clock`, a value the runner carries:
`Clock::Live` reads the machine, `Clock::Pinned` pretends it was a given time
of day when the run began and lets it run on with engine time. `snapshot` has
always simulated the clock so a clock patch can be run faster than real time;
since card 162 it can also be *aimed*, with `--time HH:MM[:SS]` on `snapshot`,
`pipe` and `play`:

```bash
# from the repo root, with --release: 30 fps of the full pipeline per second of run
cargo run --release -p screeny-art -- \
  snapshot clocks-numerals --time 21:12 --out settled.png
cargo run --release -p screeny-art -- \
  snapshot clocks-numerals --time 21:11:20 --at 38 --seed 7 --out dancing.png
cargo run --release -p screeny-art -- \
  snapshot clocks-dials --time 10:09:50 --at 15 --out telling.png
```

| you want | the command |
|---|---|
| the numerals patch **settled on 21:12**, hands holding the time | `--time 21:12` |
| it **mid-dance into 21:12**, two seconds from landing | `--time 21:11:20 --at 38 --seed 7` |
| the dials patch **telling 10:10**, gathered and held | `--time 10:09:50 --at 15` |

Reading those:

- **The settled minute is `--time` and nothing else.** With `--time`, `--at`
  defaults to 20 s and `--warmup` to the whole run: a clock has to be watched
  from its first frame, and the numerals patch dances onto the minute it was
  born on in up to about 16 s (15.4 s, the longest over 60 seeds and all
  thirteen choreographies). At 20 s every one of them has landed and settled
  and none has set off for the next minute. **No `--seed` either**: once the
  hands have landed, what is on the panel is the minute, not the dance that got
  there, and all 60 seeds give one PNG. There is a test.
- **The run starts at the pinned time and goes forward**, so `--at` is seconds
  after it, and `--at`/`--warmup` given explicitly still mean what they always
  did.
- **A dance lands exactly as the minute turns.** So the way to catch one is to
  start before the minute and render just before it: born at 21:11:20 the patch
  dances onto 21:11, holds, sets off again around t=31 and lands at t=40, which
  is 21:12:00. `--at 38` is two seconds from the end of that dance. This one
  *is* the choreography, so pin `--seed`.
- **The pinned day is day zero, not today.** The numerals patch seeds each
  minute's choreography from the absolute minute number, so "21:12 today" would
  pick a different dance tomorrow. `--time` means one thing for ever.
- **`--seed` is still the system clock by default.** Anything whose picture
  depends on the patch's own randomness wants `--seed N` as well.
- The patches' `offset` parameter is unchanged: an offset in minutes, relative
  to whatever the clock says, which is what the studio's slider wants.

`snapshot::take` is the whole run - it is what the command calls and what the
tests in [`tests/pinned_time.rs`](tests/pinned_time.rs) render through, so the
determinism claim is checked on the code that is shipped.

## Adding a patch

1. Copy `crates/art/src/patches/metaballs.rs` (continuous colour) or
   `plasma.rs` (indexed).
2. Give it a `DEF` with an id, a one-line blurb and `ParamSpec`s. The studio
   builds its controls from those: `param(..)` for a number is a slider,
   `choice(id, label, &["..", ".."], default)` for a list of named stops is a
   list, and `toggle(id, label, default)` for off-or-on is a switch (card 163).
   A choice is still an `f32` from end to end - wire, state file, per-patch
   memory - so use one whenever a parameter's values have names, rather than
   putting the key in the label. Take the names from the patch's own words.
3. Say whether it is `seeded` (card 151): does changing the seed give a person
   *another one like this*? The studio draws one quiet **Another** button from
   that and nothing at all when it is false, because the seed itself is an
   opaque number and no use to anybody. A patch that ignores its seed says
   `false`; so does one that composes as it goes and offers its own action
   through `Patch::playing` - "Compose another" is the better word, and it acts
   at once instead of rebuilding the patch.
4. List it in `ALL` in `crates/art/src/patches/mod.rs`. If it needs a GPU, list
   its id in `NEEDS_GPU` beside it, so the studio can say why it is black on a
   machine with no adapter (card 145).

Work in linear light (`Rgb`), choose colours with `color::oklch`, use
`Frame::supersample` for anything with edges or slow motion, and drive
everything from `ctx.t`. State between frames is fine; keep it in the patch.

## Clocks: numerals (`patches/clocks/`, id `clocks-numerals`)

Kinetic choreography after Humans since 1982's ClockClock 24: a 3 x 8 grid of
two-handed clocks whose hands draw the time. 8 x 8 LED cells fit the panel
exactly, and hands reach their cell edge so neighbouring strokes join into
continuous digit lines. CPU-rendered, one 16-colour ramp, exact at 4 bpp.

- **Motion is motor-limited, not tweened** (`dance.rs`, `Motor`): every hand
  shares one top speed and one acceleration, so longer moves take longer. No
  physics; this constraint is what reads as mechanical.
- **A dance is a list of phases**, each a *formation* (digits, lines, needles,
  fan, rings/spokes, compass, chevron) x a *timing* field (together, columns,
  rows, diagonal, ripple) x a *turn* rule (shortest, clockwise, counter,
  mirror, checker). Phases may overlap (negative `rest`); overlapping moves add,
  so hands cruise through a formation instead of stopping on it. Twelve dances
  ship in `dance::dance`; each is varied by a seeded RNG, and a new one is a few
  lines. A test runs every dance in many variations and checks it lands exactly
  on the time and never exceeds twice the motor speed.
- **Dances are composed, not only listed** (`dance::compose`). The twelve named
  dances are sentences in a small grammar; the composer writes new ones. Blind
  sampling of the grammar is mostly incoherent, so a composition has:
  - a *theme*: one idea of direction shared by every phase (sweep, cascade,
    diagonal, or a point). It may change once, and only after a hold, so the
    change reads as a decision;
  - an *arc*: gather into a formation, develop it with operators that suit it,
    resolve into the time. Operators: spin, quarter, open, swell, carry, and
    three that put two formations on the grid at once through a `Mask`
    (checker, columns, rows, halves, or a soft gradient band): *weave* (a
    formation with its own quarter-turn: zigzags, ladders, lattices), *split*
    (two different formations side by side) and *morph* (a band crosses the
    grid leaving the next formation behind it);
  - a *critic*: every sketch is planned for real. Hard rules reject ones that
    are too long or short, freeze the grid part-way, barely move, or overdrive
    a hand. The survivors are scored for flow, structure, pacing and
    freshness; the best of eight is performed.
  - a *memory* (`variety.rs`), because the aim is a clock that does not get
    repetitive over a day, more than one whose every dance is the best. Tags
    wear with use and recover with time, and worn sketches score lower, which
    spreads use over the whole vocabulary; a shape may not return within 30
    dances. Named dances and ambient moods are chosen the same way. A test
    simulates a day (1440 dances): 527 distinct shapes, soonest repeat after 31,
    every motif 5-12% of use and every operator 7-23%. (That test found the
    "scatter" motif starved, because it had no operators of its own; it has
    three now.)

  About 190 distinct shapes in 300 independent seeds, each continuously varied. Tests check
  that all land exactly, that the critic does not collapse onto the plain
  "direct" kind (it did, twice, while being written).
  Left to vary, six dances in ten are composed; `dance` 13 is always composed,
  1-12 are the named ones.
- **Now playing.** A patch that composes as it goes can say what it is
  performing (`Patch::playing`) and offer a control or two (`Patch::act`); the
  studio shows this in its inspector. The clock names its dance ("rings > morph
  > split, point") and offers "Play it again" and "Compose another", which
  perform at once to the time already showing. The dials patch names its mood and offers
  "Move on". (A ratings mechanism was tried and removed: the owner likes nearly
  every dance, so the composer is steered by variety, not taste.)
- **A minute has a shape**: the dance lands as the minute turns, the time is
  held ("Seconds the time is held"), then the hands are released into
  **ambient** motion (`ambient.rs`) until it is time to settle and dance again.
  Ambient cannot be planned moves, because the target never stops: each hand is
  a servo following a drifting field (a slow turn plus two ripples of unrelated
  wavelength, a separate wave for how far the two hands open, and a few degrees
  of fixed per-clock error) under the same motor limits, with a braking curve so
  it never overshoots. Four moods: drift, sway, breathe, corners. A test checks
  speed and acceleration stay bounded and that everything comes to rest.
- **A dial that is not part of a digit is still a clock** ("Resting dials", `rest`).
  `1`, `4` and `7` leave dials out of their glyph, and until card 160 those rested
  with both hands at 7:30, at full brightness: three identical strokes stacked beside
  a `1` are a colon, so `21:12` read as `2:1:12` and `14:47` as `1,4,4,7`. They must
  stay visible clock hands (the owner), so the fix is a pose and a presentation, not
  blanking. The default is **"hatched, quiet"**: hour hand at 7:30, minute hand at
  1:30 - one diagonal corner to corner, a dial reading about 1:37 - at a fifth of the
  ink. A diagonal is the one direction the digits never use, and it survives being
  looked at from across the room, where three blurred dots are a colon and three
  blurred slashes are a texture. Dimming costs no palette entry: a hand's ink scaled
  in linear light is what its own anti-aliasing ramp already is, so it lands on a step
  of that ramp and the frame stays 31 colours. The other treatments (including `0`,
  exactly what the patch did before) are switchable live so the panel can settle it -
  by name since card 163, rather than by counting a slider's stops against a legend;
  the evidence is `docs/research/010-numerals-rest-pose.md`. Resting dials only recede
  while the time is being held: a dance is always drawn at full strength, and the
  picture settles over 0.6 s as the hands land.
- Formations include non-uniform ones taken from footage of the original:
  `Flow` (hands about a bowed sine wave, folded into needles or open) and
  `Turned` (the digits with each clock's corner rigidly rotated by a wave).
- Digit shapes: 0, 2, 5, 6, 9 checked against 1080p footage of the original; 1,
  3, 4, 7, 8 follow manu.ninja's table (from the studio's promotional films).
- **To render a chosen minute**, `--time` (above): `snapshot clocks-numerals
  --time 21:12 --out x.png` is the settled picture of 21:12, the same PNG every
  time it is typed, whatever the seed. Before/after comparisons of this patch
  are only comparable that way.
- In the studio, drag "Seconds per minute" down to ~30 to see the whole cycle
  quickly, and "Choreography" to pick a dance; dances and moods are logged to
  stderr with their start times. At 60 it is a real clock on local time. Set
  "Seconds the time is held" to 60 for a clock that only moves on the minute.

## Clocks: dials (`patches/clocks/dials.rs`, id `clocks-dials`)

The same instrument as the numerals patch, with the other face. The two are a
pair, not rivals: numerals need eight columns, so 8-LED dials, and can be read
across a room; larger dials can only tell the time as analog hands, and are a
moving field first. This one is the spirit of the original rather than its
letter, adapted to the panel. A grid of two-handed dials filling all 64 x 32
(4x2, 6x3 or 8x4; 6x3 by default), in continuous motion driven by the clocks'
ambient engine. Moods never switch: their numbers glide into one another while
every oscillator keeps its own phase, so nothing ever jumps or repeats. Eight
moods (drift, sway, breathe, corners, unison, tide, rings, streamlines), which
are landmarks rather than the whole space: a mood is a handful of numbers, so
wandering glides to blends of two. Wander mode favours the open-handed ones, because hands reach their cell edge and,
when the field is gentle, neighbouring dials link into long curves across the
whole panel. The hour and minute hands each have their own hue and saturation
(`hue`/`chroma`, `hue2`/`chroma2`; the same in `clocks`), 15 steps each (31
colours, exact), drifting slowly by palette animation.

It is still a clock. Drawn digits need two dials side by side per digit, so
eight columns, so 8-LED dials: numerals and large dials cannot both fit in 64
LEDs (the `clocks` patch is the numeral version). But the dials are clocks, so
as each minute turns (`tell`), the flow gathers until every dial reads the time
as an analog clock, holds, and lets go: `Ambient::step_holding` draws every
hand to a pose exactly, under the same motor limits, and releases it back into
the field. So that the moment is not missed, while the time is held the hour
hand shortens and takes a highlight, the minute hand goes to clean white, and a
mark appears at 12 on every dial, all by palette alone. The highlight's hue is
set relative to the hands' own (`contrast`, by default the complement), because
the hands' hue drifts round the wheel and any fixed colour would sometimes be
the one they already are. Legible on 4x2, readable on 6x3.

Hands are drawn by `draw.rs`, shared with the numerals patch. Each hand's
anti-aliasing ramp is its ink scaled in linear light, which is what partial
coverage is; a ramp at constant OKLCH chroma is a different colour from the ink
dimmed (light blue cannot hold much chroma), and edge pixels used to fall onto
the other hand's ramp. There is a regression test.

## Vesta: a split-flap night clock (`patches/vesta/`, id `vesta`)

`HH:MM` on four split-flap modules and a colon, red on black, for a dark room
(card 155). CPU-rendered through `Frame::supersample`, one ramp of one hue plus
black - 32 colours, so every frame is exact on the wire.

**The layout spends the panel.** A module is 14 x 30 LEDs with its axle on the
panel's own centre line (row 16, which is a pixel boundary, so a seam of 2 is
exactly two black rows). The four centres are at x 8, 23, 41 and 56: one LED of
true black inside each pair, a 4-LED colon in the middle, columns 1..63. A
numeral is 12 x 22 LEDs of ink inside that - bowls are true circles of radius 5
so two stack exactly, and `1` is given a foot so it does not stand alone in its
module. A falling card needs rows to foreshorten through: a 15-row half-card
passes 15, 14, 12, 9, 5 and 1 rows on its way down, where a 6-row one would be
a blink.

**One cosine is the whole projection.** `squash = cos(theta - tilt) / cos(tilt)`
foreshortens the falling card; its *sign* says which face is towards us, because
the card's normal dotted with the view direction is the same cosine; and its
zero is where the card goes edge-on, which `tilt` moves past 90 degrees because
a viewer above the board sees the front of a card for longer. It is 1 at rest,
so a still module is a rectangle. Perspective widens the free edge by about a
quarter as it passes edge-on (`flap::DEPTH`); the module's window clips it, as a
real bezel would.

**What makes it read as a card and not a wipe**, in order of how much each is
worth: the next numeral's top half standing behind the falling one; the lit free
edge, brightest exactly when the face has nothing left to show; the face dimming
as it turns out of the light; and the card's shadow, which runs 1-2 LEDs *ahead*
of it down the plate below because the light sits below the eye - put the light
above and the shadow hides under the card and buys nothing.

**The fall is gravity.** `theta'' = k sin theta` with the drum's own speed as
the initial condition (`flap::PUSH`), integrated once and inverted into a table.
At the default `flip` of 0.2 s the six frames the panel gets are 0, 13, 30, 52,
84, 127 and 180 degrees: accelerating the whole way, with real motion in every
one. A card released from balance instead of pushed spends half the fall in the
first 30 degrees, which at 30 fps is three frames of nothing and then a slam.
Landing has a hint of a settle and nothing more; a card does not ring.

**Low light is few pixels at a moderate level** (card 102: large areas under
sRGB 38 sparkle). The card bodies are true black, not dark grey. The hue is
picked in OKLCH and normalised to a ray, and a channel that would land under
sRGB 7 is turned off - OKLCH's gamut search stops just inside the boundary and
leaves a thousandth of green behind at red, which is a second die lit in every
numeral pixel. At the default hue the panel gets `(x, 0, 0)` and nothing else.
Resting APL is **0.95%**, peaking at **1.23%** with four modules mid-flip;
frames are 460-620 bytes, `pal8-lz`, exact.

### Snapshots

```bash
# settled on 21:12
cargo run --release -p screeny-art -- snapshot vesta --time 21:12 --out settled.png
# the turn of four modules at once, four frames in: 09:59:59 -> 10:00:00
cargo run --release -p screeny-art -- snapshot vesta --time 09:59:59 --at 1.1333 --out flipping.png
```

With `--time 09:59:59` the snapshot steps at 30 fps from 09:59:59, so 10:00:00
lands exactly on frame 30 and a card is `n` frames into its fall at
`--at (30 + n) / 30`. At the default `flip` of 0.2 that is six frames - the
whole flip is `--at 1.0` to `1.2`, at 0, 13, 30, 52, 84, 127 and 180 degrees.
`--set flip=0.6` stretches it to eighteen, which is what makes a five-angle
study possible at all:

| flap at | `--at` (all with `--time 09:59:59 --set flip=0.6`) | actually |
|---|---|---|
| ~30 deg | `--at 1.2` | 29.7 |
| ~60 deg | `--at 1.3333` | 61.4 |
| ~90 deg | `--at 1.4333` | 97.1 |
| ~120 deg | `--at 1.5` | 127.4 |
| ~150 deg | `--at 1.5333` | 144.3 |

`the_snapshot_recipes_in_the_readme_land_where_they_say` drives a module
through the patch's own `step` and checks every row of that table, including
which frame the minute turns on. It was written against frame 31 first, and
the PNGs from that mistake were a perfectly good study of the wrong angles.

`--warmup` defaults to the whole run under `--time`, which is what a clock
needs; no `--seed`, because there is no randomness in the patch at all.

### Parameters

`light` (numeral level as an sRGB code, default 120 - card 102's sparkle floor
is 38), `hue` (0 is pure red), `size`, `weight` (stroke, LEDs), `seam`, `flip`
(a card's fall in seconds), `cascade` (flip through the numerals between, as a
real module does - on), `tilt`, `fill` (halftone, off; 0.5 halves the light
without shrinking anything), `pace`, `blink` (off: nothing in a bedroom should
blink), `hours24`, `offset`.

## Flock (`patches/flock/`, id `flock`)

Birds in slow motion, seen by a camera that is one of them. Reynolds' boids in
3D steering round invisible geometry, on the CPU, at 0.30 ms a frame. Cards 168
and 177.

- **The flock is knots and gaps, not a lattice** (card 177). Separation and
  alignment are **topological** - the seven nearest birds whatever their
  distance, which is what Ballerini et al. measured in starlings - so a dense
  patch is not pushed apart by the twenty birds behind it. Separation is soft
  except inside about a wingspan, so the spacing a bird keeps is a preference
  and not a wall. Every bird has its own preferred room and airspeed, and the
  flock is drawn into nine **clans** that fly closer to one another than to
  strangers, each clan with its own taste for room: some knots are tight and
  some are loose, which is what a broad distribution of gaps actually is.
  Nearest-neighbour distance has a coefficient of variation of 0.38-0.43, where
  the metric-radius flock of card 168 measured 0.10-0.16 - a crystal.
- **The flight goes up and down** (card 177). There is no standing spring to a
  cruise altitude any more: a bird is pulled towards the *flock's* height, so
  the flock stays one flat body that may take itself anywhere in a world 140 m
  tall. Climb and dive angles are asymmetric and open with `lift`, a dive buys
  airspeed and a climb pays for it, the wings beat harder going up, and at
  unpredictable intervals the whole flock is taken by a **surge** - a dive and
  its recovery, sometimes a climb, sometimes with a swirl through it. Both
  limits are held by a spring rather than a clamp, because a clamp on the
  velocity rotates the heading past the turn-rate limit.
- **Two controls for all of it**: `wild` (how often and how hard the flight
  changes its mind: the restlessness, the attractor's clock, how often a surge
  comes) and `lift` (how much of the motion is vertical: the climb angles, the
  height of the next attractor, the size of a surge, and how far the view may
  pitch to show it).
- **The camera is `birds[0]`.** Same rules, same speed band, same kind of turn
  limit, in every other bird's neighbour list and they in its. What it has on
  top is only what it takes to be a good seat, and each piece of it was put
  there by a measurement that failed without it:
  - it rides at the **trailing edge**, along the flock's *ground track* rather
    than its course (follow a diving flock down its own vector and the camera
    is parked above it), a little **below** the flock so the view is nose-up
    and the horizon sits low with the birds against the sky;
  - it is **more agile than a bird**, not less - a camera whose turn radius is
    larger than the flock's circle is thrown off it every time they wheel -
    and it may fly **slower**, because one that cannot fly slower than the
    flock can never drop back once it has drifted ahead;
  - it gets extra turn budget when its altitude is off the flock's, which is
    the only way to follow a dive;
  - where it **looks** is its own (smoothed) heading blended towards the
    flock, low-passed over ~0.9 s, rate-limited, and then held on a **leash**:
    the flock's middle is never more than 16 degrees off the view axis. The
    leash is on the look, not on the look's target - a low pass 50 degrees
    behind its target still shows an empty panel.
  - the view's angular rate has a **hard ceiling** (26 deg/s) that even the
    leash may not break. Vertically the leash is tighter than sideways (10
    degrees against 16), because the panel is 21 degrees tall and 38 wide.
  - it sits **ahead of the flock's vertical motion**, may climb and dive a
    quarter steeper than a bird and pays a third of what a bird pays in speed
    for a climb. A chaser held to the limits of the thing it chases arrives
    late every time, and late is the flock leaving the frame.
  - **birds before horizon** (card 177). The last three corrections to the view
    are composition, then the leash, then the rate ceiling - in that order, so
    the leash may push the horizon out of its band to keep the flock in shot,
    with an outer wall at 19 degrees so some horizon is always on the panel.
    Card 168 had composition last, which was right when nothing in the flight
    could move the horizon and wrong once something could.
- **Invisible geometry**: five blobs laid out from the seed, drifting on
  independently drawn 90-260 s periods (nothing faster, or they chase birds
  rather than being scenery), a floor, a ceiling, a soft horizontal boundary
  and a slow attractor - now picked in 3D with real height differences, on a
  clock whose *spread* is what `wild` opens up (a cubed uniform: mostly short,
  with a long tail, so there are runs of quick changes of mind and then a long
  quiet cruise). `terrain` says how many blobs are *real*, so moving it does
  not re-roll the world. The sideways part of a bird's avoidance is taken
  across the **flock's** course rather than its own heading - worked out per
  bird it sends neighbours round opposite sides of the same blob and takes the
  flock apart. A bird avoiding one may turn up to 2.2x harder
  than it cruises, as a real one does - without that the avoidance force is
  simply clipped away by the cruise turn limit and birds fly straight through.
- **Drawing a bird with almost nothing**: a body dash and two wing strokes
  whose dihedral beats, down quicker than up, each bird on its own phase, with
  a glide when it descends. Strokes go into a supersampled coverage buffer by
  bounding box, so nothing is tested against every sample. **Depth reads as
  contrast far more than as size** at 64x32: 16 m of air halves how much a bird
  stands out, and that is what stops fifty of them reading as fog.
- **The frame is indexed and exact**, through a **two-dimensional palette**: 14
  sky bands x 6 ink levels - or x 12 when the backdrop is not the sky, where
  every band is the same black and so costs nothing, and those levels are
  carrying the anti-aliasing and the whole depth cue at once. The sky band comes from the view ray's elevation
  plus the sun's glow, the ink level from the coverage buffer, and each is
  quantised through its own ordered dither (Bayer 4x4 for the sky, which
  compresses; blue noise for the ink). One consequence worth knowing: **the
  two tonal schemes are the same index image with different colours in front
  of it**, so they cost the same on the wire.
- **The sky is one ramp, brightest at the horizon**, falling off both upwards
  and downwards with a small step at the horizon itself so it is a line and not
  just the top of a gradient. The sun is the top two entries of that same ramp
  and sits low, where the sky is already at the top of it - a high sun would
  make the glow sweep through every intermediate band and draw a rainbow ring.
- **Colour rotates** (`wheel`, deg/min, 0 is off) in OKLCH at held lightness,
  so sky and birds keep their relationship and nothing flashes or goes muddy.

Two tonal schemes, and **light on dark is the one for this panel**: a white
bird two LEDs across on a near-black sky has the contrast that size cannot
give it, and it runs at 7-9% APL where dusk runs at 24%. The dusk silhouettes
are the more atmospheric picture and the horizon band is lovely, but a dark
shape on a lit sky is the weaker read at this size. Dusk wants a warm horizon:
`--set scheme=1 --set hue=35 --set spread=95`.

```bash
cargo run --release -p screeny-art -- snapshot flock --seed 11 --at 75 --warmup 75 --out level.png --scale 12
cargo run --release -p screeny-art -- snapshot flock --seed 11 --at 163.8 --warmup 163.8 --out banked.png
cargo run --release -p screeny-art -- snapshot flock --seed 11 --at 75 --warmup 75 \
  --set scheme=1 --set hue=35 --set spread=95 --out dusk.png
cargo run --release -p screeny-art -- snapshot flock --seed 11 --at 75 --warmup 75 --set near=2.5 --out close.png
# a dive and its recovery (card 177), with the sky, with only a horizon line,
# and on black. `--at` is wall-clock seconds and `pace` is 0.7, so this is the
# surge at simulated t = 45 s. Step --at by 2 for a strip of ten.
cargo run --release -p screeny-art -- snapshot flock --seed 11 --at 62 --warmup 62 --out dive.png
cargo run --release -p screeny-art -- snapshot flock --seed 11 --at 62 --warmup 62 --set backdrop=1 --out dive-line.png
cargo run --release -p screeny-art -- snapshot flock --seed 11 --at 62 --warmup 62 --set backdrop=2 --out dive-black.png
```

**`--warmup` must equal `--at`.** The flight is state: a snapshot that does not
render the whole run from zero is a different flight.

Wingbeats and camera motion only read in *sequences*, so judge it from
consecutive frames (`--at` stepped by 1/30 s or a tenth), never from a still.

Measured over three seeds x ten simulated minutes at the defaults (card 177's
numbers; card 168's in brackets where they differ): **34-38 birds in frame at
worst** [17-35] and 50 of 55 at the median, nearest bird 6.0-6.5 m and the
biggest 6.5 LEDs across, the camera 12.6-13.5 m from the flock's middle and
14.1-15.5 m at p95 [up to 21.9], nearest-neighbour distance CV **0.38-0.43**
[0.10-0.16], the flock's climb rate 2.9-3.2 m/s at p95 [2.1-2.3] over an
altitude range of 80-90 m [40], view yaw p95 14.0-15.7 deg/s [15.5-18.8], roll
p95 1.8-2.3, **pitch p95 4.0-4.9 deg/s** [2.6-3.2] with the horizon sweeping
about 15 degrees of panel where it used to sit pinned at the top of an 11
degree band, every speed and turn rate inside its limit, nothing steeper than
55 degrees, and at worst 17 m of clearance from the invisible geometry. Over
1800 frames, **not one frame of any backdrop went out lossy**: worst 1056
bytes of 1464 for the sky (light on dark), 1092 (dusk), 1182 (horizon line) and
1127 (black). The flight is identical at 30 and 60 fps, to the pixel.

## GPU and 3D patches

GPU patches render through [wgpu](https://wgpu.rs) (`crates/art/src/gpu/`). It
needs no window or event loop, so the same code runs on the studio's engine
thread (Metal on a Mac) and headless on a Linux box: Vulkan where there is a
driver, otherwise OpenGL ES 3 over EGL. `WGPU_BACKEND=gl` (or `vulkan`) forces
one; the adapter in use is logged at start-up. Device limits are held to
`downlevel_defaults`, i.e. GLES 3 class hardware.

A GPU patch is an ordinary `Patch`. It draws in linear light into an `Offscreen`
target `samples` times the panel's resolution per axis (RGBA16F + depth), and
`Offscreen::finish` reads it back and box-filters it to 64x32. Limiter, dither,
panel model and statistics are shared with CPU patches.

Two templates:

- **Shader only** (`patches/lattice.rs` + `lattice.wgsl`): write
  `fn shade(uv: vec2<f32>) -> vec3<f32>`, list the parameters, hand both to
  `ShaderPatch::boxed`. Parameters arrive as `P(0)`, `P(1)`, ... in list order;
  `u.t`, `u.seed` and `oklch()` are provided. This is the fast path for
  raymarching and Shadertoy-style experiments.
- **Mesh** (`patches/knot.rs` + `knot.wgsl`): vertex/index buffers, a camera from
  `gpu::mat`, a depth-tested pass from `Offscreen::pass`.

### Painting by palette index (`overland`)

The third template, and the one built for this panel rather than shrunk onto
it. `ShaderPatch::with_scene` takes a function that runs on the CPU every frame
and returns a `Scene`: a palette of up to 32 colours and a few floats. The
shader then never computes a colour. Each surface picks a palette entry
(`PAL(i)`), or a mix of two neighbouring entries (`ramp()`), and the frame is
mapped back onto the same palette after the downsample. Pure entries map to
themselves; mixes and anti-aliased edges become fixed blue-noise dither. The
result is a 3D scene that is an exact indexed frame.

What that buys, all of it used in `patches/overland.rs` + `overland.wgsl`:

- **Time of day is palette animation.** Geometry and indices do not care what
  hour it is; the 32 colours move through dawn, noon, dusk and night. Lossless
  and nearly free on the wire.
- **No fades through the crushed darks.** A palette colour that would fall
  below OKLCH L 0.3 is cut to true black, so at dusk the sky goes out one band
  at a time, zenith first, and night terrain is silhouette. A test checks this
  for every hour.
- **Depth without fog.** Four depth bands, each its own ramp, getting lighter,
  greyer and bluer with distance; plus occlusion, parallax and cast shadows.
- **Shapes sized for 64x32.** Terraced terrain from three noise octaves plus one
  broad one (finer detail is below an LED); flats tinted by elevation like a
  relief map; normals from a stencil that widens with distance; a sun disc
  about 8 LEDs across; monolith towers; water as horizontal dashes.
- **Small things animate in the palette.** The tower beacons pulse because
  entry 7 does.

Things to know:

- Shaders are WGSL. wgpu can also take GLSL (its `glsl` feature and
  `ShaderSource::Glsl`) if porting existing GLSL matters more than one language.
- Smoothly shaded 3D makes hundreds of colours per frame, which would take the
  lossy path. `palette::Palette` fixes that: build a palette in OKLCH
  (`Palette::ramps`), then `map` the downsampled frame onto them (nearest in
  OKLab, fixed ordered dither). The result is an indexed frame, sent exactly.
  A *shader* palette is capped at `gpu::fragment::SCENE_PALETTE` (32) by the
  uniform's array; a CPU-mapped one may go to 256 and is exact when the index
  image compresses.
  `knot` does this; set its Palette steps to 0 to compare with the raw render.
  Map after the downsample, never in the shader: averaging samples creates new
  colours.
- The Linux headless path has not been run yet (no such box on the bench). It
  needs a working Vulkan or EGL/GLES 3 driver and access to `/dev/dri/renderD*`;
  no X or Wayland session.
- `cargo build -p screeny-art --no-default-features` leaves wgpu and the GPU
  patches out (and the network stack with them; `--features gpu` keeps the GPU
  patches and drops only the network).

## Provisional assumptions

Fewer than there were. The encoding ones are gone: the sender exists, this crate
links it, and `budget.rs` - which estimated payload sizes from the brief and
faked a lossy encode with median cut and an ordered dither - was deleted by card
101. What remains:

| Assumption | Where |
|---|---|
| Transfer curve is standard sRGB | `color.rs`: `srgb_to_linear` / `linear_to_srgb` |
| The panel takes 30 fps. This stopped being an assumption in card 161: the brief measured ~30, the device's cadence ceiling is 30, and the owner asked for one rate with no variability. `screeny_art::FPS` is it, and nothing offers a choice | `crates/art/src/lib.rs`: `FPS` |
| Hand-over is raw RGB frames or palette + indices | `frame.rs`: `WireFrame`; `output/mod.rs` |
| Luminance weights are Rec.709 (panel primaries unmeasured) | `color.rs`: `Rgb::luma` |

Two more left the table in card 102, because they stopped being assumptions.

**The panel model is measured, and it is `crates/panel`.** `panel.rs` is a
reading of `screeny_panel`, not a second copy, and `screeny_panel::DEVICE` is
checked entry by entry against `firmware/src/gamma.rs`'s own table. What it
says: the firmware knows each sRGB code's wanted duty to a sixteenth of a level
and spends the remainder across sixteen successive panel refreshes, so a colour
that is held averages **1008 duty steps** per channel.

| | `Panel::BitPlanes` | the device (`Panel::Dithered`) |
|---|---|---|
| duty steps per channel | 63 | 1008 |
| distinct levels out of the 256 sRGB codes | 64 | 237 |
| codes that come out black | 22 | 2 |
| test card's dark ramp, over its 64 columns | 4 greys | 43 greys |

So **the dark end is usable**: only sRGB 0 and 1 are black, and slow fades to
black work. Two things are still true about it. A colour that is *not* held
only gets about five of the sixteen phases, which is `screeny_panel::TEMPORAL`
and 195 levels - that is what the codec chooser scores against, and it is why
large areas of very dark colour sparkle faintly rather than sitting still. And
the three channels step at different sRGB values, so low greys pick up colour
casts (brief 2.2). Dark work is a choice with a texture, not a thing to avoid.

The old "fewer levels when dimmed" was simply wrong: the device dims by
shortening the output-enable window, so every duty step survives at every
brightness and only the light goes away (card 066). A dim room is the table
above times `screeny_panel::oe_light(brightness)`.

**Dither is in duty steps, so it lands where the panel needs it.**
`Output::dither`'s bias is one duty step, which above sRGB 38 is a tenth of
an 8-bit code - it rounds away, and no noise the panel cannot show is spent on
the wire. Below 38, where up to four codes share a level, a duty step is bigger
than a code and the dither does the whole job of mixing the two levels either
side. Quantising to 64 levels, as this used to, put dither everywhere.

**Colours: 32 is a guarantee, not a ceiling.** `frame::GUARANTEED_PALETTE` (32)
is the size that goes out exactly *whatever the index plane looks like*,
because the fixed-rate `PAL5` rung is 1376 bytes for any 32 colours and any
2048 indices, noise included. `frame::MAX_PALETTE` (256) is the ceiling, and
between the two a frame is exact **when its index image compresses** - which
for flat-shaded, terraced and palette-cycled work it usually does. Nobody
models that any more: `meter.rs` runs the sender's own chooser, and
`Measured::exact` is the answer for the frame in hand. The thing to design
around is **spatial coherence, not colour count**: a 200-colour smooth gradient
fits where a 40-colour field of confetti does not.

One thing to know about the meter rather than assume: it and the sender each
hold their own `Encoder`, and the chooser gives the previous frame's codec a
small advantage. Fed the same frames they answer identically (there is a test);
if the link's cadence ceiling ever does fold a frame away - under spec 6.9 a
sender that is losing frames steps its rate down - the two histories differ and
a *marginal* frame can take a different codec.
Never a different exactness. Point the meter at the connected device with
`pipeline.meter().set_limits(lim.budget, lim.codecs)` so it is at least
measuring against the right budget.
