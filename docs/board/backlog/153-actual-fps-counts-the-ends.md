---
id: 153
title: SendStats::actual_fps() counts the frames at both ends of a stream
type: build
hardware: no
depends: []
owner:
branch:
---

## Goal

Make the frame rate the sender reports mean "the rate the stream ran at",
at any stream length.

## Context

Found by card 093 while working out why `tests/pacing.rs` failed when its run
was shortened. `SendStats::actual_fps()` is

```rust
self.frames_sent as f64 / self.started.unwrap().elapsed().as_secs_f64()
```

and `frames_sent` counts two frames that are not part of the paced window:

- the frame at slot 0, which goes out at `elapsed == 0`, so `n` slots of
  wall clock carry `n + 1` frames; and
- the `FINAL` frame `Sender::finish` transmits, which leaves immediately
  behind the last paced frame (spec 9.4 step 5) and is also counted in
  `frames_sent`.

So the figure is `(slots + 2) / (slots * period)`: high by `2 / slots`. Card
093 measured it over 50 runs on a loaded host - 30.187-30.198 fps over a 10 s
run (+0.6%), 30.310-30.325 over 6 s (+1.1%), 30.920-30.989 over 2 s (+3.2%) -
while the median interval between frames was 33.32-33.36 ms (30.00 fps) in
every one of those runs. The pacer is right; the arithmetic is not.

It is only cosmetic for a stream that runs for minutes, which is why it has
survived: `screeny stream`'s live line and its exit summary are both long
enough for 2/slots to vanish. It is wrong for anything short - an art piece
rendered as a clip, a `--frames N` run, a test - and it is the kind of number
someone will later compare against the device's own `frames_rx`.

`SendStats::mean_bytes` and `mean_encode` divide by `frames_sent` too. Those
are per-frame averages over every frame sent, so counting the `FINAL` frame
is arguably right there; decide and say so either way.

## Deliverables

- `actual_fps()` computed over the paced window: either `(frames_sent - 1 -
  final) / elapsed`, or better, from the first and last paced send, which is
  what the number means. `crates/screeny/src/sender.rs`.
- A unit or loopback test at two stream lengths that would have caught this:
  the same rate from a 2 s and a 10 s run.
- Say in the doc comment which frames are in the window and which are not.

## Acceptance

A 2 s stream and a 10 s stream at 30 fps both report 30.0 +-0.5%.

## Log
