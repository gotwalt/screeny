---
id: 090
title: Fold the sender's post-stall resync back into protocol-v1 section 9.1
type: design
hardware: no
depends: [009]
owner:
branch:
---

## Goal

Bring the spec's pacing pseudo-code into line with what the sender actually
does, so the next implementer does not reintroduce the burst it removes.

## Context

`docs/design/protocol-v1.md` section 9.1 gives a worked pacing loop, ending:

```rust
// If we have fallen more than two periods behind, resynchronise rather
// than bursting. Skipping is correct here: the device shows newest-wins.
let behind = Instant::now().saturating_duration_since(start);
let should_be = behind.as_nanos() / period.as_nanos();
if should_be as u64 > n + 1 { n = should_be as u64; }
```

Setting `n = should_be` leaves the next target at approximately *now*, so the
frame after a stall goes out immediately behind the stalled one. Card 009
measured this: a 300 ms stall at 30 fps produced two datagrams 55 microseconds
apart, which is exactly the two-frame burst the paragraph above it forbids,
and which `tests/pacing.rs::skips_rather_than_bursting_after_a_stall` fails
on. `crates/screeny` uses `n = should_be + 1` instead - resume at the next
whole slot - which costs one extra skipped frame and keeps the stream strictly
paced.

This is a one-line change to an informative section, not a wire change, so it
does not need a `txtvers`/`proto` bump. It is written up as a card rather than
edited in passing because section 9.1 is normative-adjacent: card 013 will
measure latency against it and the simulator (card 006) may assume it.

## Deliverables

- Section 9.1's snippet updated, with a sentence saying why the extra slot is
  skipped.
- A cross-reference to `crates/screeny/src/sender.rs`, which carries the same
  explanation as a comment.

## Acceptance

The spec's loop, transcribed literally, passes `crates/screeny`'s pacing
tests.

## Log
