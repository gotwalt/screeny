---
id: 183
title: The rate slider's useful stops are declared but not drawn
type: build
hardware: no
depends: [172]
owner: worker-120
branch: card/120-183-182-studio-small
---

## Goal

Card 172 replaced the two-button rate control with a slider over
`player::MIN_FPS..=MAX_FPS` and gave it a `<datalist>` of the rates worth
reaching for - 1, 10, 15, 24, 30, 60. Chrome draws tick marks for a range
input's `list` **only on the default track**, and `style.css` replaces
`::-webkit-slider-runnable-track`, so the marks are not painted. The stops are
real to the accessibility tree and to a browser that honours them; they are
invisible in the design language the page actually uses.

That is not a lie - the readout beside the slider is the exact rate, and the
arrow keys step by one, so 30 and 60 are reachable and legible. It is just less
help than was intended: at 1..60 across ~350 px a stop is about 6 px wide, and
hitting 30 with a mouse is fiddlier than it needs to be.

## Context

- `crates/studio/ui/index.html`: `#fps-slider`, `#fps-stops`.
- `crates/studio/ui/style.css`: `input[type="range"]` and its
  `::-webkit-slider-runnable-track` / `::-webkit-slider-thumb`.
- Aligning marks to values is the fiddly part: the thumb travels
  `3.5px + frac * (W - 7px)`, not `frac * W`, so a background gradient placed
  at `frac%` is out by up to half a thumb - visibly wrong at 60, which sits on
  the right edge. Whatever is drawn has to use the same mapping the thumb does.
- The same treatment would suit **Speed** (0.1..4, where 1.00x is the stop
  everybody wants) and any future `param()` whose range has a natural home
  position.

## Deliverables

- Stops drawn on the rate slider, aligned to where the thumb actually lands,
  in the page's own design language - and nothing that reaches outside the box
  (no images, no fonts).
- A decision on whether a click near a stop should settle on it. Card 172's
  worker deliberately did not snap: a magnet makes 29 and 31 unreachable by
  mouse, and the page must not quietly change a rate a script set.

## Acceptance

At 390 and 1400 px the rate slider shows where 30 and 60 are, the marks line
up with the thumb when the readout says 30 and 60, and a rate a script set is
still shown exactly (`10.5 fps` reads `10.5 fps`).

## Log
