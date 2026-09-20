---
id: 143
title: The simulator's loss-versus-slowness test counts exactly, and says little
type: test
hardware: no
depends: [117]
owner: worker-143
branch: card/143-sim-telemetry-flake
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

### Reading the code before reproducing

`crates/sim/src/device.rs` `frame_loop` is the whole mechanism: drain the frame
socket until it would block (cap `MAX_DRAIN` 256), hand each datagram to
`Core::offer_frame` **under the core lock**, `flush_frames`, then - outside the
lock - `thread::sleep(decode_ms)`. `frames_rx` is bumped inside `offer_frame`
(`crates/receiver/src/lib.rs:907`), so it is the *frame thread* that increments
it, during the drain, before the injected decode sleep. The 200 ms settle
therefore has to cover at most one decode sleep plus one drain, not sixty of
them - so hypothesis two, as the card words it ("whether 200 ms is long enough
depends on which thread increments `frames_rx`"), resolves in the test's favour
on an idle machine and only bites if the frame thread is starved for >200 ms.

Hypothesis one, the kernel receive buffer, is out on this bench by arithmetic:
`sysctl net.inet.udp.recvspace` is **786896** bytes and the stream is 60 `SOLID`
datagrams of ~11 bytes each. Even at BSD's per-datagram mbuf charge the whole
backlog is a few tens of kilobytes against a 768 KB buffer. `crates/sim`
already asserts exact counts on loopback elsewhere and does not flake there
(`tests/faults.rs`: "a clean link loses nothing on loopback", 20 of 20;
`frames_rx + frames_dropped_stale == 60` in the delayed-link test), which is the
same claim.

### Reproducing

Recipe (all bounded, all cleaned up afterwards):
`scratchpad/loadrun2.sh` - 12 spinning CPU burners plus a loop of
`cargo test -p screeny-sim -- --test-threads=16` in a second process, while the
test binary is run N times in a row by name.

- 20 of 20 pass with burners alone (load average reached **105** on a 10-core
  machine; the test's own wall time stayed 1.10-1.13 s).
- 30 of 30 pass with burners + the suite looping beside it (1.05-1.10 s).

So the failure is rarer than a loop of thirty at load 105, and hammering is not
going to find it in a reasonable time. Switched to measuring the *margins*
instead: a throwaway `crates/sim/tests/probe143.rs` (deleted afterwards) that
runs the same two streams, reads the counters at the 200 ms mark exactly as the
test does, then keeps polling until they have been still for 300 ms and prints
both. 40 runs under the same load:

| run | at the 200 ms mark | once still |
| --- | --- | --- |
| lossy | `rx=39 shown=39 sup=0 gaps=19` (40 of 40) | identical, 40 of 40 |
| slow | `rx=60 shown=8 sup=52` (28) / `shown=9 sup=51` (12) | identical |

**The 200 ms settle was never short**: at load 105 the counters were already
final at the 200 ms mark in all 80 samples, and `frames_rx` was 60 of 60 in all
40 slow runs. Both of the card's hypotheses are wrong.

### The actual cause, which was already written down

`docs/board/done/141-studio-follow-a-panel-that-moved.md` records the failure
text from the one sighting anybody captured:

> `screeny-sim`'s `a_sender_can_tell_network_loss_from_a_slow_device` ... failed
> once under a full parallel `--release` suite with **`frames_dropped_superseded
> 1, expected 0`**

That is the **lossy** run, and the assertion the card did not name:

```rust
assert_eq!(lossy.frames_dropped_superseded, 0, "a lost packet is not a superseded one");
```

`frames_dropped_superseded` is bumped in `Receiver::offer_frame`
(`crates/receiver/src/lib.rs:910`) whenever `self.pending` is already `Some` -
that is, whenever **two datagrams land in one drain of the frame loop**. The
lossy run injects no decode delay, so the drain is fast and the sender's 4 ms
gap normally puts one datagram in each. One scheduling hiccup longer than 4 ms
on the frame thread puts two in one, and the counter is 1. The assertion was a
statement about the host's scheduler, not about the device.

Reproduced the mechanism directly rather than waiting for the rare event: the
probe run at **background QoS** (`taskpolicy -b`) against 24 spinning threads
gave

```
PROBE lossy send_ms=69578 at200[rx=39 shown=35 sup=4 gaps=7 stale=0 rej=0]
```

**four superseded on a link with no slow device anywhere** - the old assertion
would have failed four times over. That is the bug, in the test.

### The fix

`crates/sim/tests/telemetry.rs`, test only; no production code touched.

- **The settle is a condition with a deadline.** After the 60 frames the faults
  come off and one more frame goes out; the test waits (`SimHandle::wait_until`,
  deadline `T` = 3 s, the file's own constant) for *that* frame to be the one on
  the panel. Loopback delivers in order and the frame thread drains in order, so
  that proves every datagram of the stream before it has been through
  `offer_frame` and counted. It replaces `sleep(200 ms)` in both runs, and its
  timeout message prints all six counters.
- **Both `superseded` assertions are proportions of `frames_rx`**, not counts
  against zero: under a quarter for the lossy run, over half for the slow one.
  Observed 0% and 85%, and 10% at the worst starvation I could produce, so the
  two cases stay an order of magnitude apart - the distinction the test exists
  for is if anything sharper than it was, because it now says *how far* apart
  they are rather than resting on a zero.
- **`frames_rx` for the slow run is "nearly all of them" plus "and `seq_gaps`
  saw no holes"**. Together those say "nothing was lost on the way in" without
  asserting an OS-dependent exact number: the tail frame proves the drain
  reached the end of the stream, so a datagram the kernel had dropped would
  necessarily have left a hole behind it and `seq_gaps` would count it.
- **A closing assertion states the test's own sentence in counters** -
  `slow.frames_rx > lossy.frames_rx` and `slow.superseded > lossy.superseded` -
  so the thing the name promises is asserted and not merely implied.
- `fn counters(&Telemetry) -> String` prints all six on one line, and **every**
  assertion message uses it.

The rewritten test is also *faster*: 0.66-0.74 s against 1.05-1.13 s, because
two fixed 200 ms sleeps became two conditions.

### The two recipes, and the before/after

**Recipe A - a loaded bench.** 12 spinning threads plus a second process
looping `cargo test -p screeny-sim -- --test-threads=16`, while the test binary
is run by name N times in a row (`scratchpad/loadprobe.sh`). Load average
reached 105 on a 10-core machine.

| | old | new |
| --- | --- | --- |
| failures | **0 of 30** | **0 of 30** |
| per run | 1.05-1.13 s | 0.66-0.74 s |

A is the recipe the exit criterion asks for and the new test is 30 for 30 under
it - but it is honest to say that **A never reproduced the bug either**, for
the old test or the new one. macOS gives a runnable thread the CPU inside 4 ms
even at load 105, and 4 ms is all this test needs.

**Recipe B - a starved bench.** The same spinning threads (24 of them) with the
test at background QoS (`taskpolicy -b`), which throttles it to near-idle and
stretches the send loop's `sleep(4)` to over a second. This *does* reproduce
the mechanism, in the counters:

| sample | lossy counters | old `superseded == 0` | new `superseded * 3 <= frames_rx` |
| --- | --- | --- | --- |
| B1 (`send_ms=69578`) | `rx=39 shown=35 sup=4` | **fails** | passes (12 <= 39) |
| B2 (`send_ms=86609`) | `rx=39 shown=32 sup=7` | **fails** | passes (21 <= 39) |

Two samples, two failures of the old assertion, on a link with **no slow device
in it at all**. That is the bug, and the new bound has better than half its
range spare at the worst starvation the bench can produce.

Recipe B also found the new test's own limit, which is now written beside the
assertion rather than papered over: one run failed with

```
and could not draw it in time; frames_rx 61, frames_shown 50, superseded 11, seq_gaps 0, ...
```

At that starvation the host had stretched the sender to ~1.4 s a frame, so 40 ms
of injected decode was no longer the slower of the two and the device drew 50 of
the 61 it got. No true assertion can call that a slow device; the test says so
with every counter printed, which is the right answer. Recipe B is ~100x
starvation - at load 105 the send loop still paced at 5.5 ms a frame.

### Checks

- `timeout 900 cargo test -p screeny-sim`: **141 passed, 0 failed** (18 binaries;
  the two long ones are `stall_234` at 40.5 s and `conformance` at 36.4 s).
- `cargo clippy -p screeny-sim --all-targets`: silent.
- `cargo fmt -p screeny-sim -- --check`: `tests/telemetry.rs` is clean. (The
  crate has 26 other rustfmt diffs, all of them pre-existing on `main` and none
  of them touched here.)
- Nothing left running: `pgrep` for the burners, the looping suite and the
  throwaway probe binary is empty, and `crates/sim/tests/probe143.rs` was
  deleted. No hardware, no serial port, no LAN - loopback only.

### Three more of the same family in `crates/sim`, not fixed here

Noted rather than touched, because the card is about one test and a small diff:

- `tests/faults.rs::a_slow_device_never_calls_the_sink_for_a_superseded_frame`
  asserts `*count == t.frames_shown` after a fixed 300 ms. The frame sink is
  called in `frame_loop` **after** the core lock is dropped, so `frames_shown`
  is already bumped while the callback has not run yet; a read landing in that
  window sees `frames_shown == count + 1`. Narrow, but real.
- `tests/faults.rs::the_frame_sink_sees_exactly_the_frames_that_reach_the_panel`
  is the same race with a 50 ms sleep over it after a proper `wait_until`.
- `tests/faults.rs::faults_can_be_turned_on_and_off_while_it_runs` and
  `a_delayed_link_raises_jitter_without_losing_frames` are exact counts after a
  fixed 100/300 ms settle - the shape this card fixed, though both counts are
  of *received* frames and so exact on loopback as long as the settle holds.

The tail-frame idea used here transplants to all four.
