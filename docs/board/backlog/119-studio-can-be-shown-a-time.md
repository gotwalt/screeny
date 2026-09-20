---
id: 119
title: The studio cannot be shown a time of day
type: build
hardware: no
depends: [162]
owner:
branch:
---

## Goal

The studio should be able to look at `clocks-numerals` at 21:12 without waiting for
21:12, the same way `screeny-art snapshot --time 21:12` can.

## Context

Card 162 gave `crates/art` the mechanism and deliberately stopped at the edge of
`crates/studio`, which another worker owned at the time. `piece::Clock` is a value a
runner carries - `Live`, or `Pinned(seconds)` carried forward by engine time - and
`Clock::parse` takes `HH:MM[:SS]` on a fixed day, so a pinned run is the same picture
tomorrow. `crates/studio/src/player.rs:290` still writes `Ctx { now: local_now(), .. }`,
which is the one line that would have to change.

Why it is worth doing: the owner is judging the clock pieces by eye, and the awkward
times (`21:12`, `14:47`, `11:11`) are the ones worth looking at. Today the only way to
see one in the studio is the `offset` parameter, in minutes relative to now, which
cannot express seconds and drifts as the session goes on.

The piece's `offset` parameter is not the answer and should not be repurposed: it is an
offset, and the studio's slider wants it to stay one.

## Deliverables

- The player holds a `piece::Clock` and passes `clock.now(t)`, not `local_now()`.
- A way to set it from the page, and a way back to the real clock. What that looks like
  is the card's design work; "seconds per minute" and the piece's own controls are next
  to it already.
- Restarting a piece (`R`) with a pinned clock restarts it *at* the pinned time, so the
  opening dance is watchable rather than a one-off.
- `crates/studio/README.md` (or the studio section of `crates/art/README.md`) says how.

## Acceptance

With the clock pinned to 21:11:50 and the piece restarted, the studio shows the dance
into 21:12 and then holds 21:12, and it shows the same thing the next time it is done.
