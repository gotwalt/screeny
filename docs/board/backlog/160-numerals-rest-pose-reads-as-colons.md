---
id: 160
title: clocks-numerals - 21:12 reads as "2:1:12"; resting dials look like colons
type: build
hardware: no
depends: [100]
owner:
branch:
---

## Goal

The time must read correctly at a glance from across the room. Today it does not whenever
a digit leaves dials unused: at 21:12 the panel reads **`2:1:12`**.

## Context

Reported by the owner on 2026-09-19 at 21:12 from the Studio running on workbench, with a
screenshot. His words: "fix how we're rendering hours that start with 2? it should be
[blank] 2[number]:[number][number], not what it is right now".

What is happening (`crates/art/src/pieces/clocks/mod.rs`): the 24 dials are 8 columns x 3
rows, four digits of 2 x 3 dials, no separator. A dial that is not part of a digit rests
with both hands at `REST = 225.0` (7:30), which the renderer draws as a short diagonal
stroke. The digit `1` (`DIGITS[1] = NN DD / NN UD / NN UU`) uses only its right column, so
its left column is three stacked diagonal strokes - which on a 64x32 panel look exactly
like a colon-ish separator. `21:12` is therefore drawn as `2 | ⋰ 1 | ⋰ 1 | 2` and read as
`2:1:12`. `4` and `7` leave rest dials too (`NN` cells). This is faithful to the original
ClockClock 24, where the rest pose is the same 7:30 - but on the original the hands are
physical objects and the digits are large; at 8 LEDs per dial the stroke is punctuation.

It is not specific to hours 20-23: any time containing a `1` (or `4`, `7`) shows it, e.g.
`10:15`, `11:11` (worst case: four fake colons). The owner noticed it at 21:xx because that
is when he looked.

**Confirm the intended reading with the owner before building** (the orchestrator asked
him; see the card Log for his answer). The orchestrator's reading of "[blank] 2[number] :
[number][number]": the unused dials should look *blank*, and the only separator-looking
thing on the panel should be a real one between hours and minutes.

## Deliverables

- A rest presentation that cannot be read as a glyph. Candidates, to be judged in the
  Studio preview and then on the panel by the owner: (a) resting dials drawn much dimmer
  than digit dials (a brightness role for "not part of the digit", which the dances can
  still animate through); (b) a rest pose that is visually minimal at this scale (hands
  overlapped pointing straight down or up reads as part of a stroke - check each against
  its neighbours); (c) both. Do not break the dances: rest is a pose the choreography
  passes through, so it must stay a valid pose for the servo model.
- If the owner wants a real hours:minutes separator, find the room honestly: 4 digits x 2
  columns fill all 8 columns, so a separator means either narrower digits, a gap column
  stolen from a `1`, or drawing it between dials in the LED grid (the grid is 64 x 24
  centred in 64 x 32; there are no spare columns, but there are spare rows). Propose, with
  snapshots (`screeny-art snapshot clocks-numerals --at ... --set offset=...`), before
  building.
- Leading zero / leading blank for hours < 10 in both 12- and 24-hour modes: decide with
  the owner and make it explicit (today `pose` always draws `hh / 10`, so 09:05 shows a
  leading 0).
- Snapshot tests for the awkward times: `21:12`, `11:11`, `10:15`, `14:47`, `09:05`,
  `00:00`, and 12-hour `1:11`.

## Acceptance

The owner reads `21:12` as 21:12 on the real panel from across the room, and `11:11` as
11:11. Indexed frames stay exact (<= 32 colours, `fallback 0`).

## Log
