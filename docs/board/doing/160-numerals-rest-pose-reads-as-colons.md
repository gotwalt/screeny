---
id: 160
title: clocks-numerals - 21:12 reads as "2:1:12"; resting dials look like colons
type: build
hardware: no
depends: [100]
owner: worker-160
branch: card/160-numerals-rest-pose
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

**The owner's answer (2026-09-19)**: "unused dials should mostly function as if they were
hands on a clock - that is to say, still visible, just in an unobtrusive rest position."
So: do **not** blank or hide resting dials, and do not dim them into invisibility. They stay
real clock hands; the job is a rest position (and, if needed, a modest presentation
difference) that the eye does not read as a colon or as part of a digit.

**And (same day)**: "clockclock24 shows [digit | 0][digit][digit | 0][digit] with no colons
or other punctuation." So the format is settled: **four digits, leading zeros kept, no
separator of any kind** - exactly what `pose` draws today. The whole defect is that the
resting dials *look like* punctuation. Do not add a colon.

## Deliverables

- A rest position that stays visible as clock hands but cannot be read as a glyph. Produce
  a contact sheet of candidates at the awkward times below and let the owner pick - on the
  panel, not only in the preview. Things to try and to be honest about: both hands
  overlapped at other angles (straight down or up sits beside the `1`'s bar and reads as
  `11`; horizontal stacks into `=`; the current 225 stacks into a colon - so look at 135,
  at angles that do not repeat identically down the column, and at the two hands slightly
  apart rather than overlapped so it reads as "a clock at rest" rather than "a stroke");
  rest dials that differ per row so three of them never line up into punctuation; a modest
  brightness or weight difference between digit hands and resting hands (modest: they must
  stay clearly visible). Do not break the dances: rest is a pose the choreography passes
  through, so it must stay a valid pose for the servo model, and a per-dial rest angle
  must not make the dances' formations asymmetric by accident.
- Keep the format: four digits, leading zero for hours < 10 (`09:05` is `0905`), no
  separator. Add a test that pins it, so nobody "fixes" it later.
- Snapshot tests for the awkward times: `21:12`, `11:11`, `10:15`, `14:47`, `09:05`,
  `00:00`, and 12-hour `1:11`.

## Acceptance

The owner reads `21:12` as 21:12 on the real panel from across the room, and `11:11` as
11:11. Indexed frames stay exact (<= 32 colours, `fallback 0`).

## Log

### 2026-09-19, worker-160: the rest pose became a switchable treatment

Read the piece first. `pose()` is the only place the digits formation is built, and
`dance.rs` gets it as `Formation::Digits`, so the rest angles are a single decision that
every dance and the servo model inherit for free. `clocks-dials` shares only `draw.rs`.

What I built, in `crates/art/src/pieces/clocks/`:

- `Rest`: a small record (hand angles, a per-row mirror flag, ink and hand-length scale)
  and a table `RESTS` of treatments, selected live by a new integer piece parameter
  `rest`. `0` is exactly what the piece did before, so the owner can A/B it on the panel.
  `Piece::playing` now carries a note naming the treatment in force, because `playing`
  cannot see parameters and the owner needs to know which one he is looking at.
- `draw::Dials` gained one field, `rest: &[[f32; 2]]`: a per-dial `[ink, length]` scale,
  empty for every caller that does not want it (so `clocks-dials` is untouched).
  **Dimming costs no palette entries**: a hand's ink scaled in linear light is exactly
  what its own anti-aliasing ramp already is, so a dim hand lands on a step of that ramp.
  The palette stays 31 colours in every treatment; there is a test.
- The resting dials only recede *while the time is being held* (`Clocks::settle`, a 0.6 s
  ease). During a dance every hand is a dancer and the grid is at full strength; the
  picture settles onto the time as the hands land. The dances are otherwise untouched.

