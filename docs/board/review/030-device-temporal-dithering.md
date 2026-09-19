---
id: 030
title: Temporal dithering in the panel driver to recover ~2 bits of depth
type: build
hardware: yes
depends: [007]
owner: claude-fable-5.1 (done inside card 007)
branch: card/007b-firmware-display
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

### Done inside card 007, on hardware (2026-09-19)

Card 007 took this as its stretch. Acceptance met; moving to `review/`.

**Where it lives.** `firmware/src/display.rs` (`render`'s `phase` argument,
`quantise_dither`, `BAYER4`) and `firmware/src/gamma.rs`. Not in the
framebuffer crate: the sub-level remainder lives in our gamma table and never
needs to reach the DMA layer, so the driver did not have to change at all.

**How.** `SRGB_TO_Q` maps each sRGB code to duty in **sixteenths of a level**
(`0..=63*16`) rather than to a level, which is the "sRGB8 -> (integer duty,
fractional remainder)" the card asked for, kept as one table and with no float
on the device. Each refresh, `quantise_dither` emits `floor(q/16)` or one more
according to whether the remainder exceeds a threshold that advances by one per
refresh. The threshold is offset per pixel by a 4x4 Bayer matrix — **not** as
spatial dithering, but so that pixels sharing a remainder do not all toggle on
the same refresh, which would beat the whole panel at refresh/16, about 10 Hz
and very visible.

**Results.**

| | |
|---|---|
| refresh rate, dithering on | **154/s measured**, unchanged from undithered |
| ... with WiFi associated and 30 fps streaming | **154/s**, 30 fps in, 0 dropped |
| black point, undithered | sRGB 34 (34 codes emit nothing) |
| black point, dithered | **about sRGB 6** |
| panel mean luminance over a 3 s clip | sd **1.7%**, no periodic structure |

Camera evidence in card 007's log: the dark ramp (sRGB 0..63 across the panel)
time-averaged over 80 frames is a continuous gradient dithered, and is **black
for 34 columns then two hard steps** undithered.
`docs/research/img/c007-darkramp-{dithered,undithered}.jpg`.

**The cost, which this card underestimated.** Dithering means the DMA
framebuffer must be rewritten **every refresh** instead of once per received
frame. The conversion is 3.1 ms and the refresh is 154 Hz, so the display task
consumes **about 48% of one core continuously — awake or idle** — against 8.7%
undithered, where it sleeps between frames. The dither arithmetic itself is
only 0.2 ms of that 3.1; the cost is the rewriting. Affordable on a dual-core
chip currently using one core, and the first thing to trade if card 008's
decode turns out to be expensive.

**A bench note that outlived the card.** A single still cannot photograph a
dithered panel honestly: the exposure is about 1/30 s and the dither cycle is
16 refreshes at 154 Hz = 104 ms, so a still catches roughly a third of a cycle
and the dark end of any pattern comes out as a checkerboard. Use a clip and
ffmpeg's `tmix` below about sRGB 40. This is in card 007's log too and belongs
in `docs/research/000-bench-notes.md` next time someone touches it.

**Not done:** the note in `docs/design/architecture.md` about how brightness
interacts. It no longer says anything interesting — card 007 made brightness
output-enable duty, which costs no bit depth, so the prerequisite this card
worried about simply went away. Recorded here instead of writing a paragraph
that only says "never mind".
