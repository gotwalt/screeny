---
id: 031
title: Make the hybrid encoder's per-frame cost fit a 30 fps sender
type: build
hardware: no
depends: [005, 009]
owner: card-009 worker
branch: card/009-sender
---

## Goal

Establish, and if necessary fix, the wall-clock cost of the card 002 hybrid
encoder so a sender can sustain 30 fps with headroom for whatever is actually
generating the frames.

## Context

Card 002 recommends a sender that encodes each frame three ways (palette+LZ
ladder, dithered PAL5, BC1_DUAL), decodes each candidate, scores it by Oklab dE
through the panel model, and transmits the winner.

Measured in `lab/` on an M-series laptop: roughly **8 ms per frame,
single-threaded**, against a 33.3 ms budget. That fits, but:

- The lab encoder is written for clarity, not speed. The palette quantiser runs
  10 Lloyd iterations over the full colour histogram; the LZ match finder walks
  a 192-deep hash chain; scoring computes three cube roots per pixel per
  candidate.
- A Raspberry Pi or similar sender is several times slower, and cards 010/011
  want the sender to also be *generating* the frames.
- Nothing here has been profiled. 8 ms is a stopwatch number from the harness,
  not a benchmark.

## Deliverables

- A criterion (or equivalent) benchmark of the encoder in `crates/screeny`,
  broken down by stage: quantise, map, LZ, block fit, candidate scoring.
- Cheap wins applied where they do not cost quality. Candidates:
  - score candidates on a subsampled pixel set, or in a cheaper space than
    Oklab (the *ranking* may not need cube roots);
  - reuse the previous frame's palette as the Lloyd seed always, not only in
    the temporal-palette mode, and cut the iteration count;
  - skip candidates that cannot win — e.g. if the frame has under 256 distinct
    colours the palette ladder is lossless and no block mode can beat it;
  - cache the histogram between the ladder and the PAL5 candidate, which
    currently build it twice.
- A documented "fast" profile that trades a stated amount of dE for speed, if
  the full chooser turns out not to fit on the target sender.

## Acceptance

A measured frames-per-second figure for the encoder on the intended sender
hardware, with the quality cost of any shortcut quantified against the card 002
numbers using the same lab metrics.

## Log
