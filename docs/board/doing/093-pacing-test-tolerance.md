---
id: 093
title: SCREENY_PACING_SECS shortens the pacing test into failing
type: test
hardware: no
depends: []
owner: worker-093
branch: card/093-pacing-test-tolerance
---

## Goal

Make `crates/screeny/tests/pacing.rs`'s `holds_thirty_fps_within_one_percent`
mean the same thing at every run length, so the documented knob for shortening
it is usable.

## Context

`crates/screeny/README.md` says "`SCREENY_PACING_SECS` shortens the ten-second
run while iterating". It does, and then the test fails:

```
SCREENY_PACING_SECS=2  -> 30.964 fps over 2 s (62 frames), outside 30.0 +-1%
SCREENY_PACING_SECS=6  -> 30.317 fps over 6 s (183 frames), outside 30.0 +-1%
(default 10 s)         -> passes, 18.4 s wall
```

Found by card 011 while running the suite; not caused by it (nothing in that
card touches `run_with`, `sleep_until` or `period_of`, and the default run
passes on the same build).

The numbers say what it is. Six seconds at 30 fps is 180 frames due and 183
were sent, so the error is a **fixed overshoot of a few frames at the end of
the run** - the stop flag is checked once per frame, and `SendStats::
actual_fps` divides by `started.elapsed()` - measured against a **relative**
tolerance. Three frames is 1.7% of 180 and 1.0% of 300, which is exactly the
boundary the default sits on. So the test is not really measuring drift at
short lengths; it is measuring how the run ends.

This is worth fixing rather than documenting away, because a ten-second test
that cannot be shortened does not get run while iterating, and the property it
pins - 30.00 fps, no drift, no burst - is one of the more valuable ones in the
crate. The 18.4 s wall time for `--test pacing` is also the single slowest
thing in `cargo test -p screeny`.

## Deliverables

- Decide what the test is measuring and measure that: most likely the frame
  *interval* over the middle of the run (mean and spread of the gaps, which
  `SendStats::min_gap`/`max_gap` already half-record), rather than a count
  divided by a wall-clock window whose ends are ragged.
- Whatever the shape, it must hold at 2 s, 6 s and 10 s, and the README's
  claim about `SCREENY_PACING_SECS` must become true.
- Keep the two properties the current test really exists for: no accumulated
  drift over the run, and no two frames closer than a third of a period.

## Acceptance

`SCREENY_PACING_SECS=2 cargo test -p screeny --test pacing` passes, as does the
default, and a deliberately drifting or bursting pacer still fails both.

## Log

### Baseline, 2026-09-19 (worker-093)

Instrumented `holds_thirty_fps_within_one_percent` to print its numbers before
any assert can abort it, then ran it 50 times on this machine with three other
workers building in parallel (load average 3.5-4.5, 10 cores). Sample sizes
fixed before the first run: 20 at the 10 s default, 20 at 2 s, 10 at 6 s. Debug
profile, one test at a time (`--exact`), 90 s timeout on each.

| run length | n  | pass | `actual_fps()` | rx `fps()` | **median gap (ms)** | drift (ms) | min gap (ms) | skipped |
|---|---|---|---|---|---|---|---|---|
| 10 s | 20 | **20** | 30.187-30.198 | 29.988-30.099 | 33.3217-33.3418 | -33.0..+4.2 | 25.2-29.3 | 0 |
| 6 s  | 10 | **0**  | 30.310-30.325 | 30.145-30.158 | 33.3228-33.3544 | -31.8..-29.1 | 22.6-29.3 | 0 |
| 2 s  | 20 | **0**  | 30.920-30.989 | 29.939-30.490 | 33.3020-33.3622 | -32.7..+4.2 | 23.2-29.9 | 0 |

That is the whole card in one table. The **median interval between frames is
33.32-33.36 ms at every run length** - within 0.1% of the ideal 33.3333 ms, 150
times tighter than the +-1% the test claims to check - while `actual_fps()`
moves from 30.19 to 30.99 purely because the run got shorter. The pacer is
identical in all 50 runs; only the measurement changes.

Confirmed causes:

- `SendStats::actual_fps()` = `frames_sent / started.elapsed()`. `frames_sent`
  counts the frame at t=0 *and* the `FINAL` frame that `finish()` transmits, so
  it is `slots + 2` over a window of `slots` periods: a fixed **+2 frame**
  overshoot, which is +0.67% at 10 s (passes, barely) and +3.2% at 2 s (fails).
  Exactly the card's diagnosis, and the numbers pin it to 2 frames, not 3.
- The receiver's `RxState::fps()` is interval-based and unbiased, but the
  `FINAL` frame lands microseconds behind the last paced frame, adding a frame
  to the count without adding to the span. Worse, it *races* `rx.shutdown()`:
  in 9 of 20 10 s runs it arrived in time to be counted and in 11 it did not,
  which is why `arrived` is bimodal (29.99 vs 30.09 at 10 s, 29.94 vs 30.49 at
  2 s). A 0.5% swing decided by a race.
- `min_gap` is 22.6-29.9 ms, never the full 33.3 ms: roughly 3 wake-ups per 10 s
  run are 4 ms late on this host, and an *absolute* schedule pays that back by
  shortening the next gap. p99 of the gaps is 37.4 ms and p1 is 29.3 ms - the
  same jitter, both signs. So `min_gap >= 0.4 * period` is measuring the OS
  scheduler, and the margin to failure is one 20 ms hiccup.
- No run skipped a frame, at any length, under this load.
