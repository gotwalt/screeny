---
id: 010
title: crates/demos - fractal zoom and word clock renderers
type: build
hardware: no
depends: [004]
owner: claude-worker-010
branch: card/010-demos
---

## Goal

Two good-looking reference pieces for the panel, as pure renderers with no networking:
an endless fractal zoom, and a word clock that spells out the time and animates its
transitions. The `screeny` CLI (card 009) will later wrap them as `screeny fractal`
and `screeny clock`; until then they are judged through a preview.

## Context

- **Read `docs/design/generative-art-brief.md` first and take it seriously.** It
  describes what this panel can and cannot show (6-bit linear levels, dark end
  missing, saturated primaries, palette/coherence-driven byte budget, sub-pixel
  motion, supersampling, wall-clock-driven animation). These two demos should be
  exemplary applications of it.
- The stock Tidbyt word clock this replaces is described with a photo in
  `docs/research/000-bench-notes.md`: three lines such as "QUARTER / TILL / TWELVE",
  upper case, blue-white on black, ~5x7 font, staircase indent, time rounded to five
  minutes and phrased colloquially.
- Frame type: sRGB RGB888, 64x32, row-major, top-left origin, `[u8; 6144]`. Also offer
  an indexed output (palette <= 32 colours + 2048 indices) where the piece is
  naturally palettised - both of these are. Define these types locally in this crate
  for now (`crates/proto` is being written in parallel and will own them; card 009
  reconciles - keep yours trivially convertible).
- Host workspace is at the repo root (`crates/*`, stable toolchain).

## Deliverables

`crates/demos` (package `screeny-demos`):

- `trait Piece { fn render(&mut self, t: Duration, out: &mut Frame); }` or similar:
  animation is a function of elapsed wall-clock time, never frame count.
- **Fractal zoom**: endless (no precision wall - e.g. loop between self-similar
  points, cycle through curated deep-zoom targets with crossfades, or use perturbation;
  your call, but it must run for hours without degenerating into blocks or a flat
  colour), supersampled and filtered in linear light, smooth iteration colouring
  through a designed palette with slow palette rotation, mid-to-bright tonal range,
  no shimmer. Deterministic from a seed.
- **Word clock**: correct English phrasing for every 5-minute slot across 12 hours
  (o'clock, five past ... half past ... quarter till; noon/midnight handling is your
  call), its own hand-made bitmap font, layout that always fits 64x32 (longest case,
  e.g. "TWENTY FIVE / TILL / ELEVEN", must fit - verify all 144 phrases fit, in a
  test), tasteful animated transition when the phrase changes (per-word or
  per-letter; only the words that change should move), subtle life between changes
  (slow colour drift or a seconds indicator) that stays within ~16 colours so frames
  are exact on the wire. Takes the time as a parameter so tests can render any moment.
- **Preview tool**: `cargo run -p screeny-demos --bin preview -- <piece> [--seconds N]
  [--at HH:MM] [--out file]` writing an animated GIF or PNG sheet **through a panel
  model** (sRGB -> linear -> 64 levels -> back; round LED dots on black with gaps,
  >= 10x upscale), so what you look at resembles the device. Look at your own output
  with the Read tool and iterate on how it looks; that is the point of this card.
- Tests: all 144 clock phrases are correct strings and fit the panel; renderers are
  deterministic; every frame's colour count is reported (assert the clock stays <= 16
  and note the fractal's typical count).
- A few representative preview images in `docs/research/img/` (< 200 KB each) and
  referenced from the card log.

## Acceptance

The previews look good to a critical eye at actual panel size, the clock is right at
every slot, and wiring either piece to a sender is a ten-line job.

## Log
