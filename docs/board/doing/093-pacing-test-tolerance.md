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
