---
id: 153
title: SendStats::actual_fps() counts the frames at both ends of a stream
type: build
hardware: no
depends: []
owner: worker-146
branch: card/146-147-153-sender-fixes
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

### Done, 2026-09-19 (worker-146)

`SendStats::actual_fps()` is now measured **from the first paced send to the
last**, the form the card preferred: `n` paced frames span `n - 1` intervals,
and neither end of the old window was a slot. Three new fields carry it -
`frames_final`, `frames_encoded`, `first_paced`/`last_paced` - plus
`frames_paced()` (`frames_sent - frames_final`). All additive; `frames_sent`
still means "datagrams on the wire, `FINAL` included", because that is what it
has always meant and what the device's `frames_rx` counts.

`SendStats`'s doc comment now states the rule for every number in it: **wire
totals** (`frames_sent`, `bytes`, `by_codec`, `mean_bytes`) include `FINAL`;
**window** numbers (`actual_fps`, `min_gap`, `max_gap`) are the paced stream
only; **encode** numbers are over the frames that were encoded.

The card's open question, decided: `mean_bytes` keeps `FINAL` (it is a real
datagram carrying a real payload, and the question it answers is what the
stream cost the network), `mean_encode` drops it (it retransmits the payload
before it and costs no encode at all, so counting it divided a real total by
an imaginary frame and reported every stream as faster to encode than it was).
That is why `frames_encoded` exists rather than reusing `frames_paced()`:
`send_frame(f, true)` encodes *and* sets `FINAL`, so the two can differ.

**Measured, release, `screeny pattern bars` into `screeny-sim` on loopback:**

| run | before (arithmetic) | after (measured) |
|---|---|---|
| 10 s | +0.67% (30.19-30.20 in card 093's 20 runs) | **29.98** (-0.07%) |
| 2 s | +3.2% (30.92-30.99 in 20 runs) | **29.89** (-0.37%) |
| 0.5 s | +13% | 29.70 (-1.0%) |

Acceptance met: 2 s and 10 s both inside 30.0 +-0.5%.

**What is left is not arithmetic, and it is worth knowing.** The residual is
*negative* and shrinks with run length: the frame at slot 0 goes out without
sleeping first, while every later frame leaves about 4 ms after its slot
because `thread::sleep` overshoots (card 154). So the span is one overshoot
longer than the slots it covers - 0.2-0.4% over two seconds, under 0.1% over
ten. That is a real latency being reported honestly rather than a miscount,
and card 154 is where it goes away. It is also why the loopback test below
asserts 1% against the nominal rate and 0.3% against the receiver's own
estimate.

Every consumer checked:

- **`screeny`'s exit summary** now reads `sent 63 frames in 2.0 s (29.89 fps
  over 62 paced)` - the count includes `FINAL`, the rate does not, and the
  line says so instead of leaving a reader to wonder why 63/2.0 is not the
  rate printed.
- **`screeny`'s live line** had the same bug in its own way: it printed frames
  since the last tick as "fps", assuming the tick interval was exactly one
  second. It now divides paced frames by the time that actually passed, and
  skips the tick that arrives a few milliseconds after `FINAL` (that was the
  stray `62 frames 1.0 fps` line at the end of every run; the summary line
  right below it says everything it could).
- **`crates/studio`** does not read `SendStats` at all - `player.rs` uses
  `LinkStats::frames_sent`, which is `Link`'s own counter and untouched.
  Nothing to do there. (Card 170 owns that crate; I did not touch it.)
- **`tests/pacing.rs`** stopped asking `actual_fps()` anything in card 093 and
  is unchanged. It passes at the default and with `SCREENY_PACING_SECS=2`.

Tests, both kinds the card allowed:

- `crates/screeny/src/sender.rs`: three unit tests over synthetic instants -
  no sleeping, so it is the arithmetic and nothing else. The rate is 30.00 at
  60, 180 and 300 slots (2 s, 6 s, 10 s) and each case also asserts that the
  **old** formula would have read high by more than 0.6% there - 31.02, 30.34,
  30.20, which is what card 093 measured on the real thing. Plus the ends one
  at a time (two paced frames are one interval; one frame is not a rate;
  nothing sent is not a rate) and the two means' denominators.
- `crates/screeny/tests/loopback.rs::the_reported_rate_is_the_same_at_any_stream_length`:
  two real streams through the sender and the in-process receiver, 20 slots at
  10 fps and 60 slots at 30 fps. **Two short runs at different rates rather
  than a 2 s and a 10 s run at the same rate**, deliberately: the bug is
  `2 / slots`, so it lives in the slot count (+10.5% and +3.4% here), while
  the noise is one wake-up's overshoot over the span, so it lives in the wall
  clock (0.2% at two seconds). That catches it harder and costs 4 s of suite
  time instead of 12. The sharp assertion is agreement with `RxState::fps()`,
  which measures the same sends from the other side of the socket with the
  same estimator (card 093) - within 0.3%, 5 runs out of 5 in release.

### Orchestrator (2026-09-20)

Merged to `main` cleanly. `cargo test --release -p screeny -p screeny-art`: green (203 passed across the
run). The only failures seen were two timing-sensitive `crates/studio/tests/fleet.rs` tests that fail
about one run in four on a loaded host and pass alone - not this branch; handed to card 170, which owns
those tests. Decisions accepted as made: resolver before browse for `.local`; `mean_bytes` keeps `FINAL`,
`mean_encode` drops it. Reaches the deployed Studio with the next deploy (card 170).
