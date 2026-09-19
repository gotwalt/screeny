---
id: 030
title: Temporal dithering in the panel driver to recover ~2 bits of depth
type: build
hardware: yes
depends: [007]
owner:
branch:
---

## Goal

Make the HUB75 driver alternate each pixel's BCM duty between adjacent values
across the panel refreshes that fall inside one received frame, so the
time-averaged output has more levels than the bit depth allows. Target: 6
bitplanes behaving like ~7.6 bits.

## Context

From `docs/research/002-frame-encoding.md` (card 002) and card 001's refresh
measurements.

- Card 001 measured 6 bits/channel at ~154 Hz on this panel; 7 bits drops to
  ~76 Hz and flickers.
- At 154 Hz refresh and a 30 fps network rate there are ~5 panel refreshes per
  received frame to dither across.
- Card 002's panel model says 6 bitplanes dithered across 5 refreshes reaches
  **195 distinct output levels from the 256 sRGB codes, versus 183 for eight
  undithered bitplanes** and 64 for six. Only 6 sRGB codes emit nothing,
  against 22 today. The table is in that report.
- This is the largest quality win card 002 found, and it is entirely in the
  firmware. The recommended codec `BC1_DUAL` already carries precision the
  panel currently throws away: its error *drops* from dE 11.01 to 10.61 when
  scored against a temporally dithered panel. Several rejected codecs get much
  worse, which is why card 002's sender picks modes scored against the
  dithered panel rather than the one we have today.
- Doing this on the *sender* at 30 fps was measured and rejected: a
  full-amplitude per-pixel pattern change at 30 Hz reads as shimmer, not
  integration (`lab/out/temporal-dither.png`). It has to happen at refresh rate.

## Deliverables

- Driver change: per-pixel duty is `floor(d)` or `ceil(d)` chosen so that the
  mean over the subframe cycle approximates the real-valued target `d`.
  A per-pixel error accumulator or a spatially offset threshold both work;
  prefer whichever costs less DMA-buffer rewriting per refresh.
- The sRGB8 -> duty table must become sRGB8 -> (integer duty, fractional
  remainder) or equivalent. Keep it a table; no float on the device.
- Measurement of the resulting refresh rate and of any visible low-frequency
  beat between the dither cycle and the 30 fps frame rate.
- Note in `docs/design/architecture.md` on how brightness interacts: card 001
  found the driver has no OE-duty brightness control, so dimming currently
  costs bit depth (~3 of 6 bits at a Tidbyt-like 30/255). Fixing that is
  arguably a prerequisite for this card to matter at normal brightness.

## Acceptance

Camera capture (card 012) of a slow dark gradient shows measurably finer
steps than the undithered driver, with no visible flicker or beating at normal
viewing distance.

## Log
