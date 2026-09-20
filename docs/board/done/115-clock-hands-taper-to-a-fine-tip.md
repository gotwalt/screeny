---
id: 115
title: Clock hands taper to a fine tip, and their edges fade in from nothing
type: build
hardware: no
depends: [102]
owner: orchestrator (software session)
branch: main
---

## Goal

The owner, 2026-09-20, on card 102's "open with the owner" list: "i'm most concerned with
hands - let's try to get a smoother, thinner hand tip".

## Context

`crates/art/src/pieces/clocks/draw.rs` draws every hand of both clock pieces: a segment
with a constant half-width and a round cap, supersampled 6x6 and mapped onto a 15-step
ramp per hand that started at OKLCH L 0.32 - because the panel was believed to have no
levels below that. Card 102 measured that it has.

## What was done

- **The ramp's floor is L 0.16, not 0.32** (`DARK`). At 0.32 the faintest edge a hand could
  have was a pixel it covers 4% of, and anything from about 2% up was rounded up to that:
  a blunt tip, and a slow hand's edge arriving in one visible step. At 0.16 the faintest
  edge is half a percent of coverage. Still 15 steps a hand, 31 colours, always exact.
- **Hands taper** (`Dials::tip`, the tip's half-width as a share of the hand's): full
  weight at the centre, finer at the tip, drawn longer by the difference so the tip ends
  where the blunt one did. A parameter on both pieces, `tip`, 0.2 to 1.0, **default 0.45**;
  1.0 is the old parallel-sided hand.
- **`clocks-numerals` tapers only while it dances.** A numeral is strokes meeting end to
  end across the cell edges, and a stroke pinched at every joint does not read as one, so
  the taper goes with `settled` and the digits are drawn exactly as before (pinned by a
  test: the settled picture is index-for-index the blunt one).
- Wire cost, from `screeny-art snapshot`, same moment, blunt against tapered: clocks-dials
  697 -> 684 bytes, `pal8-lz`, exact both times.

## Acceptance

The owner's eye on the panel. The `tip` slider on the page is there to try other values
live; the Studio remembers what it is left on.

## Log

### 2026-09-20, orchestrator

Done directly on `main` (two files' drawing code, no worker). `cargo test --release -p
screeny-art`: 60 lib tests pass, including two new ones
(`a_tapered_hand_is_finer_at_the_tip_and_no_shorter`,
`a_numeral_is_drawn_with_blunt_hands_whatever_the_tip`); clippy silent. Compared
before/after snapshots at the same moment (`--set tell=0 --set tip=1` against `tip=0.45`):
the pivot keeps its weight and the tips come to a point. Open: whether the numerals' hands
visibly "fattening" over the 0.6 s they settle reads as locking in or as a flaw - the
owner's call once he has seen it.
