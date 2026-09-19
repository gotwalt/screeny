---
id: 071
title: Score preview frames with the real encoder instead of an estimate
type: build
hardware: no
depends: [005, 009, 010]
---

## Goal

Make the preview's wire-size figure the truth rather than an estimate, and show
codec damage in the preview the way the brief's section 5 asks for.

## Context

`crates/demos/src/stats.rs` reports "distinct colours, ~N bytes, codec, APL"
under every preview tile. The colour count and APL are exact; the byte figure is
a generic LZSS estimate written in that file, because when card 010 was done
neither `crates/proto` nor the sender's encoders existed yet. It is deliberately
conservative and it disagrees with the lab: card 002 saw a Mandelbrot zoom go out
as `PAL8_LZ` on 98-100% of frames, while this estimate calls a good half of the
fractal's full-colour frames over budget. One of the two is wrong and it matters,
because it is the number an artist tunes against.

Once `crates/screeny` owns the encoders and the codec chooser (card 009), the
preview should call them:

- real encoded size and real chosen codec per frame;
- a "decoded" preview mode that renders the frame *after* encode/decode, so
  palette reduction and block-codec damage are visible as the brief intends;
- delete `lz_estimate` from `demos/stats.rs` rather than leave two answers in
  the tree.

Note for whoever picks this up: the demos' indexed path (<= 32 colours) is exact
by construction, so this only changes the story for the full-colour fractal path.

## Deliverables

- `crates/demos` preview depends on the sender's encode library for `frame_stats`.
- `preview --decoded` showing the round trip.
- The fractal's real exact-frame percentage, measured over a full tour lap, in
  this card's log and in card 010's numbers.

## Acceptance

The preview's byte figure matches what the sender actually puts on the wire, and
`lz_estimate` is gone.

## Log
