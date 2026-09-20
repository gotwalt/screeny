---
id: 143
title: The simulator's loss-versus-slowness test counts exactly, and says little
type: test
hardware: no
depends: [117]
owner:
branch:
---

## Goal

The third test in the family card 117 is about, and the one that worker could not touch:
`crates/sim/tests/telemetry.rs::a_sender_can_tell_network_loss_from_a_slow_device` has
been seen to fail once, and its assertions are exact counts that a loaded machine can
move.

## Context

Seen once by another worker, and named in card 117's note. The sim crate was owned by a
worker at the time, so 117 left it alone deliberately and carded it instead.

What the test does: sends 60 `SOLID` frames 4 ms apart into a simulator, twice - once
with 30% of the datagrams dropped on the air, once with the device taking 40 ms to decode
each one - and asserts that the counters tell the two cases apart. The second half is

```rust
assert_eq!(slow.frames_rx, sent, "decode-limited: the device got everything");
```

after a 200 ms settle. Two ways that could be a loaded machine rather than a bug, neither
verified:

- **the socket buffer.** 60 datagrams arrive in 240 ms into a device that needs 40 ms to
  draw each, so the receive queue has to hold most of them. If the kernel's receive buffer
  is smaller than the backlog, the *network* drops a frame that nothing on either side
  counts, and `frames_rx == sent` is then a statement about the OS.
- **the settle.** 200 ms is short next to 60 frames at 40 ms of decode; whether it is long
  enough depends on which thread increments `frames_rx`.

Card 117's family rule applies: the test should assert on the code's own schedule or on a
generous bound, and its failure should name the numbers. It already names `frames_rx` in
the *first* assertion, which is the pattern to copy.

## Deliverables

- The two exact assertions either shown to be exact by construction (and the reason
  written beside them), or replaced by generous ones - "nearly all of them arrived, and
  none was superseded" is the property, not "exactly sixty".
- A settle that waits for a condition with a deadline rather than sleeping a fixed 200 ms.
- Every assertion prints the counters it compared.

## Acceptance

The test passes 10 times in a row alone and once beside a `cargo build --release` into a
scratch target directory, and a failure names both counts.

## Log
