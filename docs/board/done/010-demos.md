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

### 2026-09-19, claude-worker-010, branch `card/010-demos`

Done. `crates/demos` (package `screeny-demos`) has the two pieces, the preview,
and 16 tests. Everything below was arrived at by rendering previews and looking
at them; the interesting part of this card was what looking changed.

**The preview came first** (`src/preview.rs`, `bin/preview.rs`). Panel model:
sRGB -> linear -> `--levels` steps (64 nominal, 32 for a dim room) -> back, then
each pixel as an anti-aliased dot at 58% of the pitch with a tight bloom, on the
dark LED mask the bench photo shows, composited in linear light. There is a
squint view (blurred, half scale) and a stats line under every tile: distinct
colours, estimated wire bytes and codec, APL. Extra modes that turned out to
matter: `targets` (every fractal target at several depths - the curation check),
`phrases` (the clock's awkward layouts), `probe` (measures the iteration budget
each zoom depth needs), `--bench N` (ms/frame).

Judge previews at `--scale 12` viewed at 100%: the panel is 19 x 10 cm.

**Fractal zoom** (`src/fractal.rs`). Endlessness is a *tour*: ten octaves toward
a boundary point in 90 s (~9 s per doubling), then a 1.1 s cross-dissolve into
the next leg, which is already zooming at the same rate when the dissolve
starts, so motion continuity carries the cut. Six targets, one 9-minute lap,
seeded start. Four things the previews changed:

1. **Hand-typed deep-zoom coordinates are a trap.** Three of my six ended in
   smooth exterior and faded to one flat colour by 70 s. Targets are now
   *bisected*: each region gives a point inside the set and a point outside, and
   90 halvings land on a boundary point to the last bit of f64. Black on one
   side, bands on the other, guaranteed, from endpoints that only need three
   decimals. A test re-checks every resolved target.
2. **Cycle the palette in sqrt(iterations), not iterations.** Linear cycling
   gave sub-pixel bands near the boundary: 1800 colours of pink mush at depth.
   The sqrt coordinate keeps about three cycles across the panel at any depth
   with no per-frame normalisation (which would flicker).
3. **The iteration budget decides whether a deep frame is a picture.** After
   fixing (2) every deep view came out solid black - the budget was an order of
   magnitude short. `preview probe` measured the real demand: the 90th-percentile
   pixel needs ~50 iterations at four octaves, ~5000 at twelve, ~50000 at twenty.
   Ten octaves is where 30 fps runs out, and it costs nothing artistically since
   everything deeper is finer than one of these 2048 pixels. Pixels that do
   exhaust the budget fade into the interior, so the ceiling reads as a dark halo
   rather than a rash of black speckles. Card 070 covers going deeper properly.
4. **Broad pale highlights read as dirty on LED primaries.** The six OKLCH ramps
   are saturated with a narrow highlight and a floor of L 0.40; the frame is then
   snapped to codes the panel can actually emit, which costs nothing and roughly
   halved the colour count.

**Word clock** (`src/clock.rs`, `src/font.rs`). Three staircase lines like the
stock app, hand-drawn proportional 7-row font (5 px letters, 4 for E/F/J/L, 3 for
I) - proportional because "TWENTY FIVE" is 65 px monospaced and simply does not
fit a 64 px panel; widest phrase is now 58 px. Phrasing is colloquial: past to
the half hour then till, with NOON and MIDNIGHT instead of "twelve o'clock".
Transitions are per word: at 11:40 -> 11:45 only the minutes word moves, and
words that did not change do not move at all. Two fixes from looking:

1. **The animation was firing half a slot away from the words.** `phrase` rounds
   whole minutes, so the turn-over is on a minute boundary (:03, :08, ...), not
   at the :02:30 rounding instant the transition clock assumed.
2. **Rolling the old word out and the new one in together is a smear**, not a
   transition: two words share the same seven rows for a third of a second. They
   are sequenced now - out, a beat, in - which reads like a flap board.

Life between changes: a sheen crossing every 21 s *within* the same 7-step ramp
(so it costs no colours), and a hue drift through the day. The five-minute
indicator started as a bar that filled and was, at full width, the loudest thing
on the panel; it is now a dim rail with a sub-pixel marker travelling along it,
arriving exactly as the words turn over.

**Numbers** (release, this M4, with other work running):

| | colours/frame | est. wire | ms/frame |
|---|---|---|---|
| clock | 4-8 (16 budget) | 81-267 B, always `PAL4_LZ` exact | max 0.02 |
| fractal, full colour | 95-800, mean ~390 | ~half the frames exact by the estimate | mean 6.9, p95 14, max 17 |
| fractal, indexed (<= 32) | 23-29 | 497-1140 B, always exact | max 14 |
| fractal, dissolve | - | - | max 14 |

APL: clock 1-8%, fractal 2-32%. Worst frame-to-frame luminance change: 0.014
(clock roll), 0.046 (fractal dissolve) - no flashing. Both pieces hold up at 32
levels. Supersampling is 16 samples/pixel for the first five octaves and 9 past
that (indistinguishable at depth, a third off the peak frame time); a dissolve
halves it again for its 1.1 s.

**Look at these** (rendered by the preview, through the panel model):

- `docs/research/img/010-clock.png` - QUARTER TILL TWELVE, the stock app's frame
- `docs/research/img/010-clock-roll.png` - a phrase turning over, word by word
- `docs/research/img/010-clock-phrases.png` - the longest and shortest layouts
- `docs/research/img/010-fractal-tour.png` - all six targets at three depths
- `docs/research/img/010-fractal-leg.png` - one leg, 25 s to 115 s
- `docs/research/img/010-fractal-squint.png` - the fractal from across the room

**Wiring a piece to a sender** is `Piece::render(t, &mut Frame)` for RGB888 or
`render_indexed(t, &mut Indexed)` for palette + indices; `Frame.px` is the
`[u8; 6144]` the sender wants and `Indexed` is palette + 2048 indices. Both are
pure functions of elapsed time. For the fractal, prefer the indexed path: it is
exact on the wire and, side by side, I cannot tell it from the full-colour one.

**Not happy with / open:**

- The leg dissolve is an honest cross-dissolve and for ~0.4 s in the middle you
  see two fractals at once. Shortening it to 1.1 s helped; a wipe or a
  zoom-through would be better and I did not find one I liked.
- `stats.rs`'s wire-size figure is my own LZSS estimate, and it disagrees with
  card 002 (it calls half the full-colour fractal frames over budget; the lab saw
  98-100% go out exactly). One of us is wrong. Card 071 replaces it with the real
  encoder once card 009 exists - do not tune art against that number until then.
- The fractal's first seconds of a leg are a wide flat field with a black
  silhouette. It is legitimate and reads fine, but it is the weakest moment in
  the tour.
- `crates/proto` is being written in parallel, so `frame.rs` defines `Frame` and
  `Indexed` locally as the card said; card 009 reconciles. Root `Cargo.lock` is
  deliberately not committed.
- Debug-mode `cargo test` takes ~15 s (the fractal tests); `--release` is 1 s.
