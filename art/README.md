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
