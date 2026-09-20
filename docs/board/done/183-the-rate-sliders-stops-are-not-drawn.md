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

### Drawn from the datalist, on the thumb's own geometry (worker-120)

The datalist stays - it is what the accessibility tree reads, and keeping it is
what makes this one list rather than two - and `drawStops` in `main.js` reads it
at load and puts a mark on the track for each option. Any `.slider` whose input
has a `list` gets them, so this is a mechanism rather than a special case: the
**Speed** slider the card mentions is now one attribute away (not done here;
out of this card's Deliverables).

**The geometry is `calc()`, not measurement.** Each mark is placed at
`calc(3.5px + frac * (100% - 7px))` - the same arithmetic the thumb does, with
`3.5px`/`7px` being the thumb's half-width and width from `style.css`. Nothing
is measured and nothing listens for a resize, so it is right at every width and
stays right while the two-column layout moves the slider into a narrower column.
Checked in a real Chrome, reading back the resolved positions in a same-origin
iframe (the extension's window cannot be resized):

| width | track | 1 fps | 30 fps | 60 fps |
|---|---|---|---|---|
| 390 px | 356 px wide, left 15.5 | 19.0 (= left + 3.5) | 190.5 | 368.0 (= left + width - 3.5) |
| 1400 px | 291 px wide, left 1082.5 | 1086.0 | 1225.6 | 1370.0 |

Both ends land exactly on the formula, and 30 is within 0.1 px of
`left + 3.5 + (29/59) * (W - 7)`. By eye at 390 px: five marks visible and the
thumb sitting squarely on the sixth, with the readout saying `30 fps`. Console
clean on load and after a reload.

`the_rate_sliders_stops_are_drawn_where_the_thumb_lands` in `tests/ui.rs` pins
the part that can drift silently: it pulls the thumb's width out of the
stylesheet, asserts it is still 7px, and asserts `main.js` uses that same
`3.5px + frac * (100% - 7px)`. If somebody restyles the thumb, the test says so
instead of the marks quietly going out of line at the right-hand end - which is
exactly where 60 fps is. It also checks every declared stop is a rate the player
can actually be on (`MIN_FPS..=MAX_FPS`).

**They do not snap** - the decision the card asked for, and it is card 172's
unchanged. A magnet at 30 makes 29 and 31 unreachable with a mouse, and a rate a
script set must be shown exactly rather than rounded to the nearest stop; the
readout and the arrow keys already make any rate reachable. The marks say where
the useful rates are and change nothing.

Drawn with an `<i>` each, 1 x 4 px in `var(--rule)` (the track's own colour),
lifting to `var(--dim)` while the slider is hovered. The strip is absolutely
positioned inside `.slider`, so it is out of flow and no other layout moved -
`the_page_is_in_the_order_it_should_stack_in` and
`the_two_column_layout_is_behind_a_breakpoint` are untouched and green.

### Orchestrator: merged and deployed (2026-09-20)

Merged to `main` cleanly beside card 195. Root `cargo test --release --no-fail-fast`: 717 passed, 0 failed;
clippy silent. Deployed to workbench with card 195 in one batch (state backed up; untouched). Nine seconds
after the deploy: link up on `overland` seed 4242, device facts read (fw 0.5.0, stack_free 12488, no warning
flags), `sockets` present in `/api/v1/status`. From a real Chrome tab on the deployed page (a background
window, so `document.hidden` was true and - correctly - the page asked for no pictures and its canvas
stayed black): a hand-opened socket asking `fps=30&repeat=false` received 144 frames in 4 s of 6196 bytes
each, ~218 KB/s, the last with 939 of 2048 pixels lit; no console errors. Not judged by anyone yet: the
picture drawn in a *visible* tab after this change - the owner's first glance is that check.
