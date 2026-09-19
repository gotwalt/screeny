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
