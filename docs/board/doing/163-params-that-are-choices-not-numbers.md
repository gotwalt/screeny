---
id: 163
title: parameters that are a list of choices are sliders with the list in their label
type: build
hardware: no
depends: [100, 106]
owner: worker-173
branch: card/studio-page-tells-the-truth
---

## Goal

A parameter whose values are named stops should say so, once, in a way the studio can
draw as a list rather than as a slider with the list crammed into its label.

## Context

`ParamSpec` is a number with a min, max, step and default, and several parameters are
really enumerations dressed as one. Their labels have grown to carry the key:

- `clocks-numerals`: `"Resting dials (0: as it was, 1: quiet, 2: hatched quiet, 3:
  hatched faint, 4: zigzag quiet)"` (card 160), and `"Choreography (0 = vary, 13 = always
  composed)"` - which does not even try to name the twelve.
- `clocks-dials`: `"Dials (0: 4x2, 1: 6x3, 2: 8x4)"`, `"Mood (0 = wander)"` - eight moods
  with names the piece already knows and the slider cannot show.

So the person moving the slider is reading a legend, counting stops, and in the `dance`
and `mood` cases guessing. The pieces already hold the names (`RESTS[i].name`,
`dance::named`, `Mood::new(..).name`).

Not urgent: every one of these works. It is worth doing once the studio's controls are
settled (card 106), and it would pay for itself in the clock pieces alone.

## Deliverables

- A `ParamSpec` that can carry named stops (an extra `choices: &'static [&'static str]`,
  empty for an ordinary number, is probably enough; the value stays an `f32` so nothing
  downstream changes).
- The studio draws those as a list or a segmented control, and `screeny-art list` prints
  the names.
- The clock pieces' labels go back to being labels.

## Acceptance

`screeny-art list` names every stop of `rest`, `dance`, `grid` and `mood`; the studio
shows names rather than numbers; no piece's behaviour changes.
