---
id: 166
title: The dashboard can change a panel's piece but not its parameters
type: build
hardware: no
depends: [165]
owner:
branch:
---

## Goal

A panel's player has a piece, a seed and that piece's parameters, and the dashboard shows
and can change the first two and not the third. There is no way, in a browser, to tune
what a panel is playing without going through the design view and "Play my preview".

Card 165 made the parameters worth reaching: they are remembered per piece now, and
`player/set` takes `{param: {id, value}}` and `{reset_params: true}`. Nothing in
`ui/dashboard.js` calls either.

## Context

- `crates/studio/ui/dashboard.js`: one card per device, built once and updated in place,
  with anything focused left alone. Sliders would have to follow that rule - rebuilding a
  slider under a thumb is the bug the whole file is arranged to avoid.
- `crates/studio/src/player.rs`: `PlayerChange { param, reset_params }` already does the
  work and is tested; this is a UI card.
- **Read card 170 first.** If the preview engine and the device player become one, this
  card may be the design view's own parameter panel pointed at a panel rather than a new
  set of controls on the dashboard. Do not build two.

## Deliverables

- Parameter sliders on a panel's card, from `/api/v1/bootstrap`'s specs and the player's
  current values, with a "Reset" that calls `player/set {reset_params: true}`.
- `tests/ui.rs` keeps its two guarantees: every `$('.class')` the script reaches for
  exists in the template, and every `api('...')` it calls is a route the server has.

## Acceptance

Tune a panel's piece from the dashboard, switch its piece and switch back, and the values
are as they were left - the same thing card 165 pins over HTTP, done with a thumb.

## Log
