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

### What the tests measure now

The tests record the pacer's own wake-ups - the slot it chose and the instant
`sleep_until` handed control back - through the frame source, and assert on
that `Timeline`. The split the card asked for, made explicit in the module
doc: the pacer owns **the schedule it asks for**, the OS owns **when the
thread actually wakes**, every observable is the first contaminated by the
second, and each assertion is chosen to be blind to the contamination.

| property | old assertion | new assertion |
|---|---|---|
| rate | `frames_sent / elapsed` in 29.7..30.3 | slope of lateness across the run, within 1% |
| drift | last arrival within 2 periods of due, frames counted | `span - slots * period` within 1.5 periods, slots counted |
| no burst | `min_gap >= 0.4 * period` | no gap under 0.4 of a period *behind a frame that was on time* |
| absolute grid | (implied) | no wake-up before its slot |
| skips | `skipped == 0` | skips must agree with the timeline, and stay under 1% of slots, named as host disturbance |
| receiver | `(n-1)/span` including `FINAL` | the arrivals as a `Timeline` of their own against the same slots, same estimators, `FINAL` excluded |

Three things are worth spelling out.

**A short gap behind a late frame is the schedule working.** `min_gap >= 0.4 *
period` punished exactly the behaviour spec 9.1 asks for: an absolute schedule
answers a late frame by shortening the next gap, which is the error being paid
off instead of accumulated. Since `sleep_until` never returns early, a gap can
only be short if the wake-up before it was late, and a busy host can only make
wake-ups late - so "short gap behind an *on-time* frame" is a burst the host
cannot manufacture. Measured `min_gap` was 22.6-29.9 ms on runs where nothing
was wrong.

**The median frame interval is not a usable rate estimator on this host.** It
was my first choice and 20 runs at 2 s failed 7 times once the machine got
busy (load average 13-19, against 4 during the baseline). The cause turned out
to be interesting enough to card (154): `thread::sleep` on this Mac overshoots
by about 4 ms, more than `sleep_until`'s 1 ms spin window, so a wake-up sits
either *on* its slot or 4.0 ms past it, and every flip between those two
states makes one interval long and the next one short. The intervals pile up
at three values instead of one; with 60 of them the median lands on a side
pile. It read 33.99 ms on a run whose mean interval was 33.48 ms.

`Timeline::rate_error` is the slope of lateness instead: median of the first
half of the run against median of the second. Robust to the flips, and the two
medians are half a run apart, so the estimate *sharpens* as the run
lengthens - which is what lets one tolerance hold at 2 s and at 10 s. The mean
interval is the other candidate and is rejected in the doc comment: it is
drift rewritten, a two-sample estimator (first wake-up against last) whose
noise is one wake-up's lateness however long the run is. Fine over 10 s, half
the 1% budget over 2 s.

**One stall proves nothing about bursting.** After a stall ending a fraction
`f` of the way into slot `m`, a correct pacer resumes at slot `m + 1` and so
waits `1 - f` of a period; a pacer that resumes at slot `m` fires at once. The
old test's single 300 ms stall is *exactly* nine periods, so `f` was near zero
and the correct pacer's gap was nearly a full period - it passed on an
arithmetic coincidence. Let the stall oversleep by 20 ms, which a loaded host
will do, and `f` goes past 0.6 and a **correct** pacer fails
`min_gap >= 0.4 * period`. Confirmed directly: with stalls at 9.17, 9.50 and
9.83 periods the correct pacer's smallest datagram gap is 8.2 ms, a quarter of
a period. The test now stalls three times, a third of a period apart, and
checks that the frame after each resync is *on its slot* - phase-independent,
and at most one of three phases can ever sit in the quarter where the two
pacers look alike.

### After: 50 runs, same protocol

Load average 8-14 during this set (heavier than the baseline's 4).

| run length | n | pass | rate error | drift | skipped | wake-ups held past half a period |
|---|---|---|---|---|---|---|
| 10 s | 20 | **20** | -0.022% .. +0.010% | +0.2 .. +5.1 ms | 0 | 0 |
| 6 s  | 10 | **10** | -0.028% .. +0.031% | +0.0 .. +4.0 ms | 0 | 0 |
| 2 s  | 20 | **20** | -0.122% .. +0.112% | +2.0 .. +4.0 ms | 0 | 0-1 |

Failure rate under load, `holds_thirty_fps_within_one_percent`: **30/50 before
(all 30 at 2 s and 6 s), 0/50 after.** The margin to the +-1% bound is 8x at
2 s and 30x at 10 s, and it is 8x rather than 30x for the honest reason that a
2 s run has fewer wake-ups to average.

### Mutation check

The pacer was deliberately broken three ways, one at a time, and restored;
`crates/screeny/src/sender.rs` is byte-identical to `HEAD` afterwards.

| mutation | one line | caught by | reading |
|---|---|---|---|
| **fast** `period_of(fps * 1.02)` | period 2% short | `holds_thirty_fps...` at 2 s **and** 10 s | -2.032% at 2 s, -1.966% at 10 s |
| **bursty** `n = should_be` (the card 090 regression) | resume on the slot already inside | `skips_rather_than_bursting_after_a_stall`, 3 attempts out of 3 | all three stalls flagged, post-stall lateness 0.27, 0.55, 0.85 periods |
| **drifting** `sleep_until(now + period)`, no resync | incremental schedule | `holds_thirty_fps...` and `other_frame_rates...`, both lengths | +7.5% and drift +145 ms at 2 s; +8.7% and drift +808 ms at 10 s |

The bursty pacer passes `holds_thirty_fps_within_one_percent`, and should: an
idle run never provokes the bug, because nothing ever falls two periods
behind. That is why the stall test exists and why it now stalls at three
phases. Conversely the fast and drifting pacers pass the stall test. Each test
catches what it is for, and the module doc says which is which.

### Wall time

`cargo test -p screeny --test pacing`, three runs each, debug:

- before: 18.56, 18.60, 18.71 s
- after: 18.91, 18.92, 19.03 s

+0.35 s, 1.9%, all of it the stall test going from one stall over 90 slots to
three over 100. In release the suite is 18.75 s. The 10 s default is
unchanged - the point of the card is that `SCREENY_PACING_SECS=2` now works,
which turns the suite's slowest test into a 2 s one while iterating, and the
README says so.

### Also done, and found

- `RxState::fps()` and `gaps()` counted the `FINAL` frame. `tests/cli.rs`
  asserts 29.4..30.6 fps over a 2 s stream and `FINAL` is worth +1.7% there,
  so it was sitting on the edge of the same flake. `RxState::paced()` is now
  the one place the rule lives.
- **Card 153**: `SendStats::actual_fps()` counts the frame at t=0 and the
  `FINAL` frame, so the rate the sender reports reads high by `2 / slots` -
  +0.67% over 10 s, +3.2% over 2 s. The root cause of this card, still present
  in the shipped code; nothing in this card's scope fixes it, because the
  tests no longer ask that function anything.
- **Card 154**: every frame leaves about 4 ms after the slot it was due,
  all run long, because macOS's `thread::sleep` overshoots by more than
  `sleep_until`'s 1 ms spin window. The rate is untouched - the schedule is
  absolute, so the offset is constant - which is why nothing has noticed. It
  is 12% of a frame period of avoidable latency and belongs in card 013's
  numbers.

No pacer bug found. Code and `docs/design/protocol-v1.md` 9.1 agree; the one
spec sentence this work casts doubt on is "spinning the last millisecond ...
is what makes the difference between 30 fps give or take 5 ms and 30.00 fps",
whose rate half is measured and true and whose latency half is card 154's
question, so 9.1 is left alone until that card answers it.

Root `cargo test --release --no-fail-fast`: 300 tests, 52 binaries, all pass.
