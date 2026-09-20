# art: generative art for the screeny panel

Design brief: [`docs/design/generative-art-brief.md`](../../docs/design/generative-art-brief.md).
Read it first; this code is that brief turned into a pipeline.

Two crates in the repo's single workspace (`crates/`; host toolchain, not the `esp` one):

| | |
|---|---|
| `crates/art` (`screeny-art`) | Library + headless binary. Pieces, panel model, dither, limiter, statistics, outputs. No GUI dependencies; this is what will run on a server. |
| `crates/studio` (`screeny-studio`) | Tauri v2 desktop app for designing pieces. A window onto the same pipeline, drawn as LEDs. |

Nothing here opens the serial port or implements the wire protocol - that is
`crates/proto` and `crates/screeny`, and this crate calls them. Frames leave
through the `Output` trait (`crates/art/src/output/`), of which
[`SenderOutput`](src/output/sender.rs) is the real one.

## Run

```bash
# from the repo root
cargo run -p screeny-studio                      # the designer
cargo run -p screeny-art -- list                 # pieces and their parameters
cargo run -p screeny-art -- pipe plasma | ...    # raw 6144-byte sRGB frames on stdout, 30 fps
cargo run -p screeny-art -- snapshot plasma --seed 7 --at 6 --out plasma.png
cargo test -p screeny-art                        # includes the end-to-end wire tests

# to a panel (use --release, the encoder is ~10x slower in a debug build)
cargo run --release -p screeny-art -- play plasma --to screeny-4a00a4

# the network-free build: no sockets, no mdns-sd, no `play`
cargo build -p screeny-art --no-default-features            # also no GPU pieces
cargo build -p screeny-art --no-default-features --features gpu
```

## Sending to a panel

`play <piece> --to NAME|ADDR` streams a piece over UDP through
[`screeny`](../screeny)'s `Link`. `--to` takes an mDNS instance name - preferred,
because the link re-resolves it on every reconnect and so follows the device
across a DHCP lease - or an `IP[:PORT]`. `--wait` starts without a panel and
picks one up when it appears; `--seconds` bounds the run; ctrl-c sends `FINAL`
so the panel is released at once.

Three things are worth knowing before building on it:

- **Indexed frames go on the wire exactly.** Up to 32 colours whatever the
  indices, up to 256 when they compress. Pixel-exactness is checked end to end
  against `screeny-sim` in [`tests/sender.rs`](tests/sender.rs).
- **Render at whatever suits the piece.** The link never sleeps and never
  bursts: it applies the device's cadence ceiling itself, so a 60 fps piece into
  a 30 fps panel puts 30 on the wire and the device supersedes nothing. Half the
  frames come back `Coalesced`, which is the system working.
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
(add `--features gpu` to keep the GPU pieces). It has no sockets, no mdns-sd and
no ctrlc, the core - pieces, pipeline, meter, preview - is all there, and
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
piece -> limiter -> quantise to panel levels (ordered dither) -> WireFrame -> outputs
                                                             \-> meter: real encode
                                                                      -> real decode -> preview
```

- A **piece** turns (wall-clock `t`, seed, parameters) into a `Frame`: either
  linear-light RGB, or a palette of up to 32 colours plus indices. Indexed frames
  pass through exactly; prefer them.
- The **limiter** caps average picture level and the rate at which mean
  luminance (and mean red) may rise, so no piece can strobe the panel.
- The **meter** (`meter.rs`) runs the sender's own chooser and the firmware's
  own decoder over every frame, so the codec, the byte count, the exactness
  decision and the preview picture are measured rather than estimated. The
  preview is literally the decoded datagram.
- The studio's four meters are the four numbers from brief section 5.

Pieces that tell the time read `ctx.now` (local time of day), not the system
clock: the studio and `pipe` pass the real time, `snapshot` simulates it, so
clock pieces can be run faster than real time and tested.

## Adding a piece

1. Copy `crates/art/src/pieces/metaballs.rs` (continuous colour) or
   `plasma.rs` (indexed).
2. Give it a `DEF` with an id, a one-line blurb and `ParamSpec`s. The studio
   builds its sliders from those.
3. List it in `ALL` in `crates/art/src/pieces/mod.rs`.

Work in linear light (`Rgb`), choose colours with `color::oklch`, use
`Frame::supersample` for anything with edges or slow motion, and drive
everything from `ctx.t`. State between frames is fine; keep it in the piece.

## Clocks: numerals (`pieces/clocks/`, id `clocks-numerals`)

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
- **Now playing.** A piece that composes as it goes can say what it is
  performing (`Piece::playing`) and offer a control or two (`Piece::act`); the
  studio shows this in its inspector. The clock names its dance ("rings > morph
  > split, point") and offers "Play it again" and "Compose another", which
  perform at once to the time already showing. The dials piece names its mood and offers
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
  exactly what the piece did before) are switchable live so the panel can settle it;
  the evidence is `docs/research/010-numerals-rest-pose.md`. Resting dials only recede
  while the time is being held: a dance is always drawn at full strength, and the
  picture settles over 0.6 s as the hands land.
- Formations include non-uniform ones taken from footage of the original:
  `Flow` (hands about a bowed sine wave, folded into needles or open) and
  `Turned` (the digits with each clock's corner rigidly rotated by a wave).
- Digit shapes: 0, 2, 5, 6, 9 checked against 1080p footage of the original; 1,
  3, 4, 7, 8 follow manu.ninja's table (from the studio's promotional films).
- In the studio, drag "Seconds per minute" down to ~30 to see the whole cycle
  quickly, and "Choreography" to pick a dance; dances and moods are logged to
  stderr with their start times. At 60 it is a real clock on local time. Set
  "Seconds the time is held" to 60 for a clock that only moves on the minute.

## Clocks: dials (`pieces/clocks/dials.rs`, id `clocks-dials`)

The same instrument as the numerals piece, with the other face. The two are a
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
LEDs (the `clocks` piece is the numeral version). But the dials are clocks, so
as each minute turns (`tell`), the flow gathers until every dial reads the time
as an analog clock, holds, and lets go: `Ambient::step_holding` draws every
hand to a pose exactly, under the same motor limits, and releases it back into
the field. So that the moment is not missed, while the time is held the hour
hand shortens and takes a highlight, the minute hand goes to clean white, and a
mark appears at 12 on every dial, all by palette alone. The highlight's hue is
set relative to the hands' own (`contrast`, by default the complement), because
the hands' hue drifts round the wheel and any fixed colour would sometimes be
the one they already are. Legible on 4x2, readable on 6x3.

Hands are drawn by `draw.rs`, shared with the numerals piece. Each hand's
anti-aliasing ramp is its ink scaled in linear light, which is what partial
coverage is; a ramp at constant OKLCH chroma is a different colour from the ink
dimmed (light blue cannot hold much chroma), and edge pixels used to fall onto
the other hand's ramp. There is a regression test.

## GPU and 3D pieces

GPU pieces render through [wgpu](https://wgpu.rs) (`crates/art/src/gpu/`). It
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
  pieces out (and the network stack with them; `--features gpu` keeps the GPU
  pieces and drops only the network).

## Provisional assumptions

Fewer than there were. The encoding ones are gone: the sender exists, this crate
links it, and `budget.rs` - which estimated payload sizes from the brief and
faked a lossy encode with median cut and an ordered dither - was deleted by card
101. What remains:

| Assumption | Where |
|---|---|
| Transfer curve is standard sRGB | `color.rs`: `srgb_to_linear` / `linear_to_srgb` |
| 64 linear levels per channel (fewer when dimmed) | `panel.rs`: `NATIVE_LEVELS`; a runtime setting everywhere else |
| The panel takes 60 fps (the brief measured ~30; the owner says to assume 60). The studio engine and `pipe` default to 60, with 30 selectable | `crates/studio/src/main.rs`: `RATES`; `screeny-art pipe --fps` |
| Hand-over is raw RGB frames or palette + indices | `frame.rs`: `WireFrame`; `output/mod.rs` |
| Luminance weights are Rec.709 (panel primaries unmeasured) | `color.rs`: `Rgb::luma` |

One thing to know about the meter rather than assume: it and the sender each
hold their own `Encoder`, and the chooser gives the previous frame's codec a
small advantage. Fed the same frames they answer identically (there is a test);
under the link's default cadence ceiling a 60 fps piece sends every other frame,
so the two histories differ and a *marginal* frame can take a different codec.
Never a different exactness. Point the meter at the connected device with
`pipeline.meter().set_limits(lim.budget, lim.codecs)` so it is at least
measuring against the right budget.
