---
id: 032
title: Re-validate the codec choice against real photo and video content
type: research
hardware: no
depends: [005]
owner:
branch:
---

## Goal

Confirm that card 002's recommendation survives content that was not written by
the person choosing the codec.

## Context

Every clip in `lab/src/content.rs` is synthetic: procedural plasma, a
Mandelbrot zoom, a hand-authored UI mock, a procedural "photo" and a dark-end
probe. They were written to probe specific failure modes and they did — but
they are the lab's own content, and the conclusions are load-bearing for the
v1 wire format.

The two conclusions most exposed to this are:

- **Palette+LZ beats the block codecs on smooth saturated motion.** That rests
  on frames typically having under ~1000 distinct colours, which is true of
  procedurally generated content and of anything downscaled with a box filter,
  but may not be true of camera footage with sensor noise. If real frames have
  1500-2048 distinct colours, the 256-colour palette stops being nearly
  lossless and `BC1_DUAL` may take over.
- **389 bytes for a text frame.** That depends on the UI mock's 7 colours.
  Real Tidbyt apps use antialiased text and gradients; the colour count could
  be an order of magnitude higher.

## Deliverables

- A loader in `lab/` for PNG sequences / GIFs / short video clips, downscaling
  to 64x32 in linear light (the existing supersample path already does the
  filtering correctly).
- A small corpus checked in or scripted-to-fetch, covering at minimum: camera
  video with grain, a real Tidbyt-style app screenshot sequence with
  antialiased text, and a high-motion clip.
- The card 002 results table regenerated over the combined corpus.
- An update to `docs/research/002-frame-encoding.md` — either confirming the
  recommendation or amending it, with the numbers.

## Acceptance

The recommended codec set in the protocol design is backed by measurements on
content nobody in this project authored. If the ranking changes, the wire
format's mode list changes with it before card 005 freezes it.

## Log
