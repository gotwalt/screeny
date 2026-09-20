---
id: 197
title: The Speed slider has no home position
type: build
hardware: no
depends: [183]
owner:
branch:
---

## Goal

Speed runs 0.1 to 4.0 in steps of 0.05, and 1.00x - "play it as the piece
intended" - is a stop everybody wants and nobody can hit with a mouse. It is
one of 79 positions, and the only way back to it is the keyboard.

## Context

Card 183 drew the rate slider's stops and left the mechanism general: any
`.slider` whose range input has a `list` gets its stops drawn, from the datalist
itself, at the thumb's own geometry. So this is a `<datalist>` in
`crates/studio/ui/index.html` and nothing else - `drawStops` in `main.js`
already does the rest.

What still needs deciding is **which** stops. 1.00x certainly; perhaps 0.5x and
2x. Card 183's decision not to snap holds here too and for the same reason: a
magnet at 1.00 makes 0.95 and 1.05 unreachable by mouse, and the studio must not
quietly change a speed a script set.

The open question this one really raises is whether a slider with an obvious
home position wants a way *back* to it - a double-click, or the reset the
parameter block has - rather than only a mark. Worth an owner's opinion before
building anything.

## Deliverables

- A datalist on `#speed-slider`, with stops worth reaching for.
- The `ui.rs` test extended to cover it, or made general over every slider that
  declares stops.

## Acceptance

The Speed slider shows where 1.00x is, at 390 and 1400 px, aligned with the
thumb; a speed a script set is still shown exactly.

## Log
