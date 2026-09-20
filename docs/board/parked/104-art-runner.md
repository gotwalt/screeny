---
id: 104
title: A runner that keeps the panel interesting: rotate and schedule pieces
type: design
hardware: no
depends: [101]
owner:
branch:
---

## Goal

Today one process runs one piece. The owner's stated aim for the clock pieces is that
they "not get repetitive"; the same applies one level up. Design, with the owner, how
a long-running service chooses what is on the panel: a playlist, time-of-day rules
(a clock in the morning, `overland` in the evening?), transitions between pieces, and
how parameters and seeds are configured without the studio.

## Context

`crates/art/src/variety.rs` already does this within the clock pieces (wear by
tag, no near repeats) and may generalise. Transitions must respect the limiter.

## Deliverables

A short design note in `docs/design/`, agreed with the owner, then build cards.

## Log

### Parked by the owner, 2026-09-20

"i don't care about scheduling or transitions" - he wants to go back to focused aesthetic
work on the pieces. Not to be picked up, and the cross-fade-or-cut question goes with it,
until he raises it.
