---
id: 002
title: Frame encoding lab - best picture in one 1472-byte datagram
type: research
hardware: no
depends: []
owner: worker-002
branch: card/002-frame-encoding-lab
---

## Goal

Find the encoding(s) that give the best perceived image on a 64x32 RGB LED panel when
every frame must fit in a single UDP datagram (<= 1472 bytes payload, minus a small
header, budget ~1450 bytes for pixels), decode on a 240 MHz ESP32 in well under 5 ms,
at 30 fps. Back the recommendation with measurements, not opinion.

## Context

- 64x32 = 2048 pixels. Raw RGB888 is 6144 bytes, so we have ~5.6 bits/pixel.
- Content classes we care about: (a) visualizer / algorithmic art - smooth gradients,
  saturated colour, full-frame motion (plasma, fractal zoom); (b) text and UI - few
  colours, hard edges, mostly static, must be pixel-exact; (c) downscaled video/photo
  as a stress case.
- The panel is driven with binary-coded modulation; effective depth is likely 5-8
  bits/channel *before* gamma, so low-end precision is limited and LED response is
  roughly linear (needs gamma correction). Find out what depth is realistic and take
  it into account: there is no point sending precision the panel cannot show.
- Temporal effects are available: at 30 fps network rate and a much higher panel
  refresh, temporal dithering (on the sender, or on the device between frames) may
  buy effective depth.
- Loss model: newest frame wins, no retransmit. Encodings that depend on the previous
  frame (deltas) must tolerate a lost packet (e.g. periodic keyframes, or stateless
  only). Weigh that.

## Candidates to evaluate (add your own)

- Fixed global palette, 4 bpp and 5 bpp, with/without ordered or error-diffusion dither.
- Per-frame adaptive palette (median cut / k-means / octree) at 4 and 5 bpp; palette
  in-packet; temporal palette stability (flicker when the palette shifts frame to frame).
- Block-based: DXT1/BC1-style 4x4 blocks (2 x RGB565 + 2-bit indices = 4 bpp = 1024
  bytes flat), block truncation coding, variable block palettes.
- Chroma subsampling: YCbCr/YCoCg 4:2:0 with reduced bit depth per plane.
- Reduced direct colour: RGB565 is too big (4096); RGB332/RGB444 + dither with a
  cheap entropy stage?
- Generic byte compressors small enough for an MCU (RLE, LZ4-style, heatshrink,
  a tiny Huffman/rANS) layered over any of the above; worst-case size matters because
  a frame that does not fit must degrade gracefully, not be dropped.
- Hybrid / per-frame mode switch: header byte selects codec so text/UI can go
  lossless-RLE-palette and visualizer frames can go lossy.

## Deliverables

- `lab/` : a std Rust crate (workspace-independent is fine) with
  - synthetic test content generators for classes (a) and (b) (plasma, a mandelbrot
    zoom, scrolling text/UI mock with a bitmap font) and a loader for (c) from image
    files/GIFs if cheap to add;
  - each candidate codec as `encode(&Frame, budget) -> Vec<u8>` / `decode(&[u8]) -> Frame`;
  - a harness that reports per codec x content: bytes (mean/max, % frames over budget),
    PSNR and a perceptual-ish metric (e.g. SSIM or error in a gamma-aware / Oklab
    space), temporal flicker metric, and host-side decode time as a proxy (note
    instruction-level complexity for the ESP32: no FPU-heavy or big-table decoders);
  - PNG contact sheets (upscaled nearest-neighbour) so a human can eyeball results,
    written under `lab/out/` (git-ignored) with a few representative ones copied to
    `docs/research/img/`.
- `docs/research/002-frame-encoding.md`: conclusions first - the recommended codec
  set and packet budget, a results table, what was rejected and why, and estimated
  ESP32 decode cost.

## Acceptance

A reader can pick the v1 wire format pixel encoding(s) from the report without
re-running anything, and `cargo run --release` in `lab/` reproduces the numbers.

## Log

### Worker, branch `card/002-frame-encoding-lab`

**Delivered**

- `lab/` — std Rust crate. `cargo run --release` (~8 s) regenerates every
  number and PNG; `cargo test --release` runs 7 round-trip tests.
  - `lab/src/dec/` — 16 decoders, `no_std`, allocation-free, integer-only.
    `lab/nostd-check` compiles exactly that tree as `#![no_std]` with no
    allocator, so "firmware-ready" is checked by the compiler, not asserted.
  - `lab/src/enc/` — 22 codecs in the measured roster.
  - `lab/src/content.rs` — plasma, Mandelbrot zoom, text/UI mock (with a 5x7
    bitmap font), a photo-like stress case, and a dark-end probe. Everything
    but the UI mock is rendered at 4x and box-filtered in linear light.
  - `lab/src/panel.rs` — panel model: sRGB EOTF, BCM bit depth, optional
    device-side temporal dithering across N refreshes.
  - `lab/src/metrics.rs` — panel-aware Oklab dE, a blurred "viewing distance"
    dE, SSIM, pixel-exactness, flicker, 4-frame-average error.
  - `lab/xtensa-bench/` — counts real Xtensa instructions per decode under
    `qemu-system-xtensa`.
- `docs/research/002-frame-encoding.md` + five contact sheets in
  `docs/research/img/`.
- New cards: 030 (device temporal dithering), 031 (sender encode budget),
  032 (real content corpus).

**Recommendation**: a per-frame mode chooser over `PAL4_LZ` / `PAL8_LZ` /
`PAL5` / `BC1_DUAL`. Mean panel-aware dE 6.53 against 9.52 for the best
fixed-rate codec, 1029 B mean / 1464 B max, worst-case decode 131 k Xtensa
instructions (~0.55-0.82 ms, 2.5% of a frame). Byte layouts are in the report.

**Surprises**

1. Palette+LZ beat every block codec on smooth saturated motion — PAL8_LZ was
   selected for 98-100% of `plasma` and `mandel` frames. At 2048 pixels a frame
   usually holds under ~1000 distinct colours, so a 256-entry palette is nearly
   lossless. The tiny frame size inverts the texture-compression literature's
   answer.
2. Endpoint precision, not index count, decides dark content. `bc1` and
   `bc1-e888` differ only in RGB565 vs RGB888 endpoints and the latter is 2.9x
   better on `darkfade`. Visible as magenta blotches in near-black sky.
3. Scoring against the panel we have is a trap. RGB444-endpoint codecs
   (`blk42`, `cc4`) look competitive today and collapse under a temporally
   dithered panel (dE 10.6 → 35.0): the panel's coarseness is hiding them.
   The hybrid now selects against the *better* panel, costing 0.12 dE today.
4. Six bitplanes dithered across five panel refreshes gives 195 distinct output
   levels against 183 for eight undithered bitplanes. The cheapest big quality
   win in the project is in the panel driver — card 030.
5. At today's dimmed brightness (~3 linear bits), every codec in the study
   scores dE 0.00 on the dark clip. The panel shows black regardless.
6. Ordered dither loses on every number and wins to the eye in exactly one
   place (4 bpp gradients). The blurred-dE metric exists because the contact
   sheet disagreed with the table.

**Notes for the orchestrator**

- Card 001's panel measurement arrived mid-card and changed the scoring; the
  writeup is against a 6-bit panel throughout, with 3-bit/8-bit/dithered
  columns alongside.
- Budget used is 1464 B (8-byte header), per the message from the orchestrator.
- Mode bytes used so far are listed in `lab/src/dec/mod.rs::mode`; card 004
  should reserve that space in the protocol spec. Only five of the sixteen are
  recommended for v1.
- No hardware was touched.
