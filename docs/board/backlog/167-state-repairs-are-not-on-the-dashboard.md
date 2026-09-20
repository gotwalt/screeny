---
id: 167
title: What the studio had to correct in the state file is not shown anywhere
type: build
hardware: no
depends: [165]
owner:
branch:
---

## Goal

Card 165 added `state.repaired` to `/api/v1/status`: the remembered values this build
could not use as written and silently corrected - a parameter a piece no longer has, one
outside a range that has changed, one a human typed as a string. It is said once in the
log at startup and it is in the JSON, and the dashboard's server card does not draw it.

That is the one place it is wanted. The log line scrolls away in a container that has been
up for a month; the dashboard is what somebody looks at when a piece "came back wrong".

## Context

- `crates/studio/src/state.rs`: `StoreHealth.repaired`, capped at 16 sentences.
- `crates/studio/ui/dashboard.js`: `drawServer()` already draws `state.path`,
  `state.writes` and `state.last_error` into the server card's `<dl>`.
- It is **not a fault**: a value being out of range after a piece was re-ranged is exactly
  what the memory is meant to survive, so this must not make `/healthz` 503 and must not
  be drawn in the `bad` tone. `dim`, or a plain note.

## Deliverables

- A row (or a short list) on the dashboard's server card, and the `tests/ui.rs` element
  check extended to it.

## Acceptance

Start a studio on a hand-edited state file with a parameter a piece does not have; the
dashboard says so in words, and still says the server is well.

## Log
