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

The rewritten test is also *faster*: 0.65-0.74 s against 1.05-1.13 s, because
two fixed 200 ms sleeps became two conditions.
