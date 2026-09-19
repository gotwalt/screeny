---
id: 092
title: An indexed FrameSource for the built-in demos, and four cargo doc warnings
type: build
hardware: no
depends: [011, 016]
owner:
branch:
---

## Goal

Two small pieces of `crates/screeny` tidying that card 011 deliberately left
alone, because doing them needs `crates/demos`, which card 016 held at the
time.

## Context

Card 011 added `Pixels`, `Sender::send_indexed` and `Link`, so an *embedder*
can put an indexed frame on the wire exactly. The built-in demos cannot: they
are `FrameSource`s, which render into an RGB `Frame`, so the word clock - eight
colours, drawn from a palette it already has - is expanded to 6144 bytes and
then has its palette rediscovered by the encoder's histogram one frame later.
The picture is identical (the ladder finds the same colours and `PAL4_LZ`
carries them losslessly), so this is about honesty and about 0.05 ms, not about
quality.

Card 011's note: adding the seam without a consumer would have been dead API,
and the only natural consumer is `crates/demos`, which was being refactored in
parallel.

Separately, `cargo doc -p screeny --no-deps` emits four warnings, all
pre-existing and all the same mistake: `crates/screeny/src/encode/quant.rs`
links `[`SEED_DRIFT`]` and `[`RESEED_EVERY`]` from public documentation, and
both constants are private (lines 23, 29, 154, 155).

## Deliverables

- An indexed variant of the `FrameSource` seam - most likely a second trait
  (`IndexedSource`) rather than a method on `FrameSource`, so no existing
  implementor changes - and a `Sender`/`Link` path that takes it.
- `crates/demos`' word clock implemented against it, since it is already
  palette-authored. The fractal is a continuous-colour renderer and should stay
  RGB.
- A test that the clock's frames arrive bit-exact through `screeny-sim` and
  that the encoder never sees an expanded frame (assert on
  `SendStats::indexed_exact`).
- `quant.rs`: either make the two constants public, or unlink them. They are
  genuinely part of the explanation, so public is probably right.

## Acceptance

`cargo test -p screeny -p screeny-demos` passes; `cargo doc -p screeny
--no-deps` is warning-free; `screeny clock` sends `PAL4_LZ` with
`indexed_exact` rising and `indexed_fallback` at zero.

## Log
