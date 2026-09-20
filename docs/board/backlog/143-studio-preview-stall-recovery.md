---
id: 143
title: The design view's engine has no stall recovery
type: build
hardware: no
depends: [106]
owner:
branch:
---

## Goal

A device player that stalls is caught by a watchdog, abandoned and replaced (card 106).
The design view's engine is not: it is card 105's single `Engine` behind one
`std::sync::Mutex`, so a piece that stops returning holds that lock for ever. `/healthz`
goes 503 and `/api/v1/status` still answers - card 106 made sure of both - but every
other route blocks, and only a restart clears it.

## Context

The fix is the shape the device players already have: the render core separate from
everything else, so it can be thrown away. `AppState::engine` would become a cell the
handlers read through, and the supervisor would replace a wedged core with one on the
fallback piece, rebuilt from the persisted preview state - which card 106 already keeps.

The cost is touching all thirteen of card 105's routes, which is why card 106 did not
do it: it is a mechanical change that should not be mixed into a behavioural one.

## Deliverables

- A swappable preview core, with the same watchdog, fallback ladder and fault brake the
  device players have.
- `tests/fleet.rs::a_wedged_preview_is_a_503_and_the_dashboard_still_answers` becomes
  "a wedged preview is replaced, and the 503 clears by itself".
- All of card 105's tests still green.

## Acceptance

Set the design view to `fault-stall` with `SCREENY_STUDIO_FAULTS=1`: within ten seconds
the page is drawing again, on the fallback piece, with a line in the log.

## Log
