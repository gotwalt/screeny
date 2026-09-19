# art: generative art for the screeny panel

Design brief: [`docs/design/generative-art-brief.md`](../docs/design/generative-art-brief.md).
Read it first; this code is that brief turned into a pipeline.

Two crates in one workspace (host toolchain, not the `esp` one):

| | |
|---|---|
| `screeny-art/` | Library + headless binary. Pieces, panel model, dither, limiter, statistics, outputs. No GUI dependencies; this is what will run on a server. |
| `studio/` | Tauri v2 desktop app for designing pieces. A window onto the same pipeline, drawn as LEDs. |

Nothing here talks to the device, opens the serial port, or implements the wire
protocol. Frames leave through the `Output` trait (`screeny-art/src/output.rs`).

## Run

```bash
cd art
cargo run -p screeny-studio                      # the designer
cargo run -p screeny-art -- list                 # pieces and their parameters
cargo run -p screeny-art -- pipe plasma | ...    # raw 6144-byte sRGB frames on stdout, 30 fps
cargo run -p screeny-art -- snapshot plasma --seed 7 --at 6 --out plasma.png
cargo test -p screeny-art
```

The studio needs no Node toolchain and no `tauri-cli`: the front end is three
static files in `studio/ui/`, embedded at build time. Edit them and re-run.

Studio keys: `Space` pause, `R` restart, `N` new seed, `1` `2` `3` LEDs / squint / pixels.

## Pipeline

```text
piece -> limiter -> quantise to panel levels (ordered dither) -> WireFrame -> outputs
                                                             \-> lossy sim -> preview
```

- A **piece** turns (wall-clock `t`, seed, parameters) into a `Frame`: either
  linear-light RGB, or a palette of up to 32 colours plus indices. Indexed frames
  pass through exactly; prefer them.
- The **limiter** caps average picture level and the rate at which mean
  luminance (and mean red) may rise, so no piece can strobe the panel.
- The studio's four meters are the four numbers from brief section 5.

Pieces that tell the time read `ctx.now` (local time of day), not the system
clock: the studio and `pipe` pass the real time, `snapshot` simulates it, so
clock pieces can be run faster than real time and tested.

## Adding a piece

1. Copy `screeny-art/src/pieces/metaballs.rs` (continuous colour) or
   `plasma.rs` (indexed).
2. Give it a `DEF` with an id, a one-line blurb and `ParamSpec`s. The studio
   builds its sliders from those.
3. List it in `ALL` in `screeny-art/src/pieces/mod.rs`.

Work in linear light (`Rgb`), choose colours with `color::oklch`, use
`Frame::supersample` for anything with edges or slow motion, and drive
everything from `ctx.t`. State between frames is fine; keep it in the piece.

## Twenty-four clocks (`pieces/clocks/`)

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
    a hand. The survivors are scored for flow, structure, pacing, freshness
    and (lightly) taste; the best of eight is performed.
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
  "direct" kind (it did, twice, while being written), and that ratings steer it.
  Left to vary, six dances in ten are composed; `dance` 13 is always composed,
  1-12 are the named ones.
- **Taste** (`taste.rs`). Every dance carries tags (`motif:rings`, `op:weave`,
  `theme:point`, `mask:band`...). In the studio's **Now playing** panel, "More
  like this" / "Less like this" nudge the weight of each tag the dance carried;
  a sketch's taste score is the mean weight of its tags. One dance in five
  ignores taste so there is always something new to rate. "Play it again" and
  "Compose another" perform at once, to the time already showing. Weights are
  saved to `~/.screeny-art/clocks.taste` (or `$SCREENY_ART_HOME`), a plain
  `tag<TAB>weight` file, so the headless runner shares the studio's taste by
  copying it. Any piece can offer such a panel: see `Piece::playing` / `act`.
- **A minute has a shape**: the dance lands as the minute turns, the time is
  held ("Seconds the time is held"), then the hands are released into
  **ambient** motion (`ambient.rs`) until it is time to settle and dance again.
  Ambient cannot be planned moves, because the target never stops: each hand is
  a servo following a drifting field (a slow turn plus two ripples of unrelated
  wavelength, a separate wave for how far the two hands open, and a few degrees
  of fixed per-clock error) under the same motor limits, with a braking curve so
  it never overshoots. Four moods: drift, sway, breathe, corners. A test checks
  speed and acceleration stay bounded and that everything comes to rest.
- Formations include non-uniform ones taken from footage of the original:
  `Flow` (hands about a bowed sine wave, folded into needles or open) and
  `Turned` (the digits with each clock's corner rigidly rotated by a wave).
- Digit shapes: 0, 2, 5, 6, 9 checked against 1080p footage of the original; 1,
  3, 4, 7, 8 follow manu.ninja's table (from the studio's promotional films).
- In the studio, drag "Seconds per minute" down to ~30 to see the whole cycle
  quickly, and "Choreography" to pick a dance; dances and moods are logged to
  stderr with their start times. At 60 it is a real clock on local time. Set
  "Seconds the time is held" to 60 for a clock that only moves on the minute.

## Hands (`pieces/hands.rs`)

The clocks without the time: the spirit of the original rather than its
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
LEDs (the `clocks` piece is the numeral version). But the dials are clocks, so
as each minute turns (`tell`), the flow gathers until every dial reads the time
as an analog clock, holds, and lets go: `Ambient::step_holding` draws every
hand to a pose exactly, under the same motor limits, and releases it back into
the field. While held, the hour hand shortens and, by palette alone, deepens to
amber while the minute hand pales to white. Legible on 4x2, readable on 6x3.

## GPU and 3D pieces

GPU pieces render through [wgpu](https://wgpu.rs) (`screeny-art/src/gpu/`). It
needs no window or event loop, so the same code runs on the studio's engine
thread (Metal on a Mac) and headless on a Linux box: Vulkan where there is a
driver, otherwise OpenGL ES 3 over EGL. `WGPU_BACKEND=gl` (or `vulkan`) forces
one; the adapter in use is logged at start-up. Device limits are held to
`downlevel_defaults`, i.e. GLES 3 class hardware.

A GPU piece is an ordinary `Piece`. It draws in linear light into an `Offscreen`
target `samples` times the panel's resolution per axis (RGBA16F + depth), and
`Offscreen::finish` reads it back and box-filters it to 64x32. Limiter, dither,
panel model and statistics are shared with CPU pieces.

Two templates:

- **Shader only** (`pieces/lattice.rs` + `lattice.wgsl`): write
  `fn piece(uv: vec2<f32>) -> vec3<f32>`, list the parameters, hand both to
  `ShaderPiece::boxed`. Parameters arrive as `P(0)`, `P(1)`, ... in list order;
  `u.t`, `u.seed` and `oklch()` are provided. This is the fast path for
  raymarching and Shadertoy-style experiments.
- **Mesh** (`pieces/knot.rs` + `knot.wgsl`): vertex/index buffers, a camera from
  `gpu::mat`, a depth-tested pass from `Offscreen::pass`.

### Painting by palette index (`overland`)

The third template, and the one built for this panel rather than shrunk onto
it. `ShaderPiece::with_scene` takes a function that runs on the CPU every frame
and returns a `Scene`: a palette of up to 32 colours and a few floats. The
shader then never computes a colour. Each surface picks a palette entry
(`PAL(i)`), or a mix of two neighbouring entries (`ramp()`), and the frame is
mapped back onto the same palette after the downsample. Pure entries map to
themselves; mixes and anti-aliased edges become fixed blue-noise dither. The
result is a 3D scene that is an exact indexed frame.

What that buys, all of it used in `pieces/overland.rs` + `overland.wgsl`:

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
  lossy path. `palette::Palette` fixes that: build up to 32 colours in OKLCH
  (`Palette::ramps`), then `map` the downsampled frame onto them (nearest in
  OKLab, fixed ordered dither). The result is an indexed frame, sent exactly.
  `knot` does this; set its Palette steps to 0 to compare with the raw render.
  Map after the downsample, never in the shader: averaging samples creates new
  colours.
- The Linux headless path has not been run yet (no such box on the bench). It
  needs a working Vulkan or EGL/GLES 3 driver and access to `/dev/dri/renderD*`;
  no X or Wayland session.
- `cargo build -p screeny-art --no-default-features` leaves wgpu and the GPU
  pieces out.

## Provisional assumptions

The firmware, protocol and sender are still being built. Everything this code
assumes about them is confined to these places, so reconciling is a small edit:

| Assumption | Where |
|---|---|
| Transfer curve is standard sRGB | `color.rs`: `srgb_to_linear` / `linear_to_srgb` |
| 64 linear levels per channel (fewer when dimmed) | `panel.rs`: `NATIVE_LEVELS`; a runtime setting everywhere else |
| <= 16 colours = 1072 bytes exact, <= 32 = 1376 exact, more = lossy; 1464-byte budget | `budget.rs` |
| What a lossy encode looks like (median cut + ordered dither; a stand-in, not the sender's algorithm) | `budget.rs`: `simulate_lossy` |
| The panel takes 60 fps (the brief measured ~30; the owner says to assume 60). The studio engine and `pipe` default to 60, with 30 selectable | `studio/src/main.rs`: `RATES`; `screeny-art pipe --fps` |
| Hand-over is raw RGB frames or palette + indices | `frame.rs`: `WireFrame`; `output.rs` |
| Luminance weights are Rec.709 (panel primaries unmeasured) | `color.rs`: `Rgb::luma` |

When the sender library exists, it becomes an `Output` impl and replaces
`budget.rs`'s estimates with real encoded sizes.
