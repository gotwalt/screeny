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

## Adding a piece

1. Copy `screeny-art/src/pieces/metaballs.rs` (continuous colour) or
   `plasma.rs` (indexed).
2. Give it a `DEF` with an id, a one-line blurb and `ParamSpec`s. The studio
   builds its sliders from those.
3. List it in `ALL` in `screeny-art/src/pieces/mod.rs`.

Work in linear light (`Rgb`), choose colours with `color::oklch`, use
`Frame::supersample` for anything with edges or slow motion, and drive
everything from `ctx.t`. State between frames is fine; keep it in the piece.

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
| Hand-over is raw RGB frames or palette + indices | `frame.rs`: `WireFrame`; `output.rs` |
| Luminance weights are Rec.709 (panel primaries unmeasured) | `color.rs`: `Rgb::luma` |

When the sender library exists, it becomes an `Output` impl and replaces
`budget.rs`'s estimates with real encoded sizes.
