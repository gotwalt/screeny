---
id: 154
title: sleep_until's 1 ms spin window is narrower than macOS's sleep overshoot
type: research
hardware: no
depends: []
owner:
branch:
---

## Goal

Find out whether the 4 ms every frame currently spends waiting past its slot
is worth buying back, and how.

## Context

Measured by card 093 while rebuilding `tests/pacing.rs` around the pacer's own
wake-ups. At 30 fps on this Mac, under load and idle alike:

```
lateness min 0.00 ms  median 3.4 ms  p95 4.0 ms  max 4.0-17 ms
```

where lateness is how far past `start + n * period` the pacer actually woke.
It is not drift and it is not jitter: the *intervals* are 33.333 ms and the
run's rate is within 0.03% of 30 fps. It is a constant **phase offset**. Every
frame leaves about 4 ms after the moment it was due, all run long, and 4 ms is
12% of a frame period.

The cause is in `sleep_until` (`crates/screeny/src/sender.rs`, spec 9.1):

```rust
const SPIN: Duration = Duration::from_millis(1);
...
if let Some(coarse) = left.checked_sub(SPIN) { std::thread::sleep(coarse); }
while Instant::now() < target { std::hint::spin_loop(); }
```

`thread::sleep` on macOS lands about 4 ms late - timer coalescing, which the
kernel applies generously to a thread with no QoS class - so the coarse sleep
overshoots the point where the spin was supposed to take over and the spin
never runs. The loop is then self-correcting at the *rate* (the next target is
absolute, the next sleep is shortened by the same 4 ms, the steady state is a
constant offset) which is why nothing has ever noticed. The distribution is
bimodal: a wake-up is either on its slot or 4.0 ms past it, and a run flips
between the two states a few times. Card 093 had to stop using the median
frame interval as a rate estimator because of those flips.

Spec 9.1 says spinning the last millisecond "costs roughly 3% of one core at
30 fps and is what makes the difference between 30 fps give or take 5 ms and
30.00 fps". The rate half of that claim is right and measured. The latency
half is not true on this host, and the spec should say what it is worth after
this card answers it.

Three things to weigh, cheapest first:

1. **Widen `SPIN`** to 5 ms. Costs about 15% of one core at 30 fps instead of
   3%, which on a laptop sending frames is probably fine and on a Raspberry-Pi
   class box running the Studio (`docs/design/studio-vision.md`) is probably
   not.
2. **Ask the OS for a better timer.** macOS honours a QoS class
   (`QOS_CLASS_USER_INTERACTIVE`) and `thread_policy_set` with
   `THREAD_TIME_CONSTRAINT_POLICY` for exactly this; Linux has
   `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME)`, which does not have the
   problem in the first place. Measure before adding a dependency - the
   `spin_sleep` crate the spec mentions does not solve this either.
3. **Leave it.** 4 ms of a 33 ms budget matters only if the end-to-end latency
   budget is tight, which is card 013's question, and the panel's own refresh
   is not synchronised to us anyway.

Whatever the answer, it belongs in card 013's latency numbers: 4 ms of the
glass-to-glass figure is host-side and avoidable.

## Deliverables

- A measurement of `thread::sleep` overshoot on this Mac against sleep length
  and against thread QoS, in `docs/research/`.
- The same for whatever Linux box the Studio is headed for, if one is to hand.
- A decision recorded in `docs/design/protocol-v1.md` 9.1, replacing the
  "30.00 fps" sentence with what was measured.

## Acceptance

Either the median lateness in `tests/pacing.rs`'s report drops below a
millisecond, or the card says in one paragraph why 4 ms is the right price.

## Log

### Parked 2026-09-20 (owner: "let's prune what doesn't need to be done"; focus is aesthetic work)

A constant 4 ms phase offset in the pacer; the rate is right and nobody can see a phase. Not worth buying back.
