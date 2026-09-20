---
id: 080
title: A wire-level conformance suite the firmware can be run against too
type: test
hardware: no
depends: [006, 008]
owner: worker-080
branch: card/080-conformance-suite
---

## Goal

Card 006's receive-rule tests all drive `screeny-sim` in process. The firmware
(card 008) is the third implementation of the same spec and the one that
actually matters, and there is currently no way to ask it the same questions.
Lift the rules half of those tests into a suite that talks to *any* endpoint
over UDP, and run it against both.

## Context

- `crates/sim/tests/{sequence,arbitration,control,telemetry,malformed}.rs`
  already say what the rules are. They use `SimHandle::snapshot()` to check
  state the wire does not carry, which is precisely the part that does not port.
- Everything those tests assert about the wire is reachable over the wire:
  `TELEMETRY` carries all eight counters and the state byte, `BUSY` says who
  holds the lock, and `GET_INFO` says what the device claims to be. A
  conformance run can be written entirely in terms of those three.
- What cannot be checked over the wire is bit-exactness of the displayed frame.
  That needs the camera harness (card 012) on hardware, or `--dump-dir` against
  the simulator.
- `crates/sim/tests/common/mod.rs` is already a sender with an encoder per
  codec; it should move somewhere both can use it rather than being copied.

## Deliverables

- A crate or test target that takes `--addr host:frame_port` and a control
  port, runs the suite, and reports pass/fail per rule with the spec section
  cited.
- It must be safe to point at the real device: no `SET_WIFI`, no `REBOOT`, no
  flashing, and it must restore brightness and idle mode on exit.
- `crates/sim`'s own tests keep their in-process assertions (bit-exact pixels,
  events) and drop the duplicated wire-level ones.

## Acceptance

The suite passes against `screeny-sim` on loopback, and the orchestrator can
run it against the real device with one command and get a rule-by-rule report.

## Log

### 1. What is covered where, before any code (2026-09-19)

The card's Context predates cards 008, 016 and 101. Re-read against today's tree:

- `screeny-probe` already has `conformance` (22 checks) and `lock-test` (11
  checks), so this card is **not** a new tool. The roadmap says grow the suite
  from the probe, and that is what the work below does.
- `crates/receiver` is the one receive core the simulator and the firmware
  share (card 016), so "the third implementation" is now the firmware's
  *plumbing* around a shared core rather than a third decoder. That does not
  make the suite pointless - the plumbing is exactly what only a device can get
  wrong (which socket answers, which datagram is counted, what the radio does
  to a 2000-byte datagram) - but it does mean a failure is more likely to be in
  the firmware task structure than in a decoder.
- `crates/sim/tests/core_rules.rs` did not exist when this card was written. It
  drives `Core` on a virtual clock and is *not* wire-level duplication: it
  asserts things the wire cannot express (LOCK_MS to the microsecond, HOLD_MS
  without waiting ten seconds, two senders on different IPs). It stays whole.

Legend: **P** = `screeny-probe conformance`/`lock-test` today, **S** = asserted
in `crates/sim/tests/*` over loopback, **C** = asserted in
`crates/sim/tests/core_rules.rs` on the virtual clock. "Wire?" is whether an
external client can see it through TELEMETRY counters + the state byte, BUSY,
GET_INFO or a control reply.

| # | Rule | Spec § | P | S | C | Wire? |
|---|---|---|---|---|---|---|
| 1 | datagram > 1472 discarded and counted, not parsed from its prefix | 1 | - | y | - | yes, **loopback only** (the radio never delivers it) |
| 2 | bad magic -> discard + `frames_rejected` | 2.1 | y | y | - | yes |
| 3 | version != 1 on the frame port -> discard + counted | 2.2 | y (v9) | y (v0, v2) | - | yes |
| 4 | reserved packet type (0x1 `FRAME_FRAG`, 0xF) -> counted | 2.2 | - | y | - | yes |
| 5 | `CONTROL` on the frame port -> counted, never answered | 2.2 | y | y | - | yes |
| 6 | `FRAME` on the control port -> discarded, **not** counted, not answered | 2.2 | - | y | - | yes |
| 7 | datagram shorter than a header -> counted | 2.2 | - | y | - | yes |
| 8 | `8 + len > datagram` -> counted | 2.3 | y | y | - | yes |
| 9 | `len > 1464` on a FRAME -> counted | 2.3 | - | y | - | yes |
| 10 | bytes beyond `8 + len` are padding, frame still shown | 2.3 | - | y | - | yes |
| 11 | `HAS_TS` with no room for the timestamp -> counted | 3 | - | y | - | yes |
| 12 | `HAS_TS` parsed and skipped; pixels unaffected | 3 | - | y | - | partly (shown, `last_codec`); bit-exactness is in-process |
| 13 | reserved frame flag bits ignored, not rejected | 3.1 | - | y | - | yes |
| 14 | `KEY` clear still decodes in v1 | 3.1 | - | y | - | yes |
| 15 | `seq` wraps; 0 is newer than 0xFFFF | 3.2 | - | y | - | yes |
| 16 | duplicate / past / half-space-away -> `frames_dropped_stale` | 3.2 | y (dup) | y (all four) | - | yes |
| 17 | a gap adds `seq.wrapping_sub(last)-1` to `seq_gaps`, frame still shown | 3.2 | y (gap 5) | y (+ across the wrap) | - | yes |
| 18 | a new source resets `last_seq`, counters keep running | 3.2/7.3 | - | y | - | yes |
| 19 | `rx = shown + superseded + decode` | 3.3 | y | y | y | yes |
| 20 | a whole drain is counted, only the survivor shown | 3.3 | - | y (via faults) | y | partly (needs a slow decoder to provoke) |
| 21 | every advertised codec decodes a spec-built payload | 4.1-4.6 | only via `stream` (no assert) | y, bit-exact | - | yes for "was shown + `last_codec`"; **not** bit-exactness |
| 22 | reserved / unsupported codec id -> `frames_dropped_decode` | 4.7 | y | y | - | yes |
| 23 | wrong-length payload -> `frames_dropped_decode`, `rejected == 0` | 4 / 4.7 | y (SOLID +-1) | y (5 codecs) | - | yes |
| 24 | trailing bytes after the LZ stream -> decode drop | 4.4 | - | y (`PAL8_LZ` stub) | - | yes |
| 25 | an undecodable frame leaves the previous one lit | 4.7 | - | y | - | no (pixels) |
| 26 | `GET_INFO` body is the TXT wire format, required keys, `txtvers` first | 5.2/6.6 | `info` cmd only | y | - | yes |
| 27 | TXT `codecs` = all five; `ctrl` is reachable; `mtu` = 1464 | 5.2/4.7 | `info` cmd only | y | - | yes |
| 28 | `GET_INFO` one reply per source per second | 5.5 | y | y | - | yes |
| 29 | ... except a repeat of an answered `req_id` | 5.5 | y | y | - | yes |
| 30 | `req_id == 0` -> silence, error replies included | 6.1 | y | y | - | yes |
| 31 | a `CONTROL` with `REPLY` set is discarded | 6.1 | y | y | - | yes |
| 32 | a reply echoes `op` and `req_id` | 6.1 | - | y | - | yes |
| 33 | unsolicited `TELEMETRY`/`BUSY` carry `REPLY` and `req_id == 0` | 6.2 | - | y | y | yes |
| 34 | unsolicited `TELEMETRY` rate-limited to 100 ms per source | 6.2 | - | y | y | yes |
| 35 | a `TELEMETRY` *request* is never rate-limited | 6.2 | y | y | - | yes |
| 36 | `BUSY` rate-limited to 1 s per source | 6.2 | y | y | y | yes |
| 37 | `BUSY` reason `LOCKED`, `remaining <= LOCK_MS` | 6.2/6.3 | printed, not asserted | y | y (exact) | yes |
| 38 | `STATS_REQ` answered on the **frame** port | 6.4 | y (implicit) | y | y | yes |
| 39 | `STATS_REQ` answered even when the frame is stale or undecodable | 3.1/6.2 | - | - | y | yes |
| 40 | a locked-out sender gets `BUSY`, not telemetry | 6.2 | - | - | y | yes |
| 41 | every opcode implemented | 6.3 | partly (9 of 13) | y | - | yes |
| 42 | unknown opcode -> `ERR_UNKNOWN_OP` | 6.3 | y (1 op) | y (7 ops) | - | yes |
| 43 | `ERR_BAD_LENGTH` for a body that is not the opcode's size | 6.5 | y (2 cases) | y (9 cases) | - | yes |
| 44 | `ERR_BAD_ARG` for the right size, the wrong value | 6.5 | y (2 cases) | y (8 cases) | - | yes |
| 45 | `ERR_VERSION` for an unknown version on the control port | 2.2/6.5 | y | y | - | yes |
| 46 | `SET_BRIGHTNESS` reports what was applied (the cap) | 6.3 | y, **but leaves the panel at 96** | y (cap 120) | y | yes |
| 47 | `REBOOT` without the magic -> `ERR_BAD_ARG`, no reboot | 6.3 | y | y | - | yes |
| 48 | `GET_WIFI` never returns the PSK | 8.4 | `wifi` cmd only | y | - | yes |
| 49 | `TELEMETRY` body is exactly 48 bytes | 6.7 | - | y | - | yes |
| 50 | `interarrival_us`/`jitter_us` track a paced stream | 6.8 | printed, not asserted | y | - | yes |
| 51 | `RESET_STATS` zeroes the counters | 6.8 | used, not asserted | y | y | yes |
| 52 | ... and leaves uptime, `last_codec`, state, the lock and `last_seq` alone | 6.8 | - | y (partly) | y | yes |
| 53 | first frame adopts a source -> `LIVE` | 7.3/7.4 | y | y | - | yes |
| 54 | a second source inside `LOCK_MS` -> `frames_rejected` + `BUSY` | 7.4 | y | y | y | yes |
| 55 | takeover at `LOCK_MS` | 7.4 | y | y | y (exact) | yes |
| 56 | a displayed `FINAL` releases the lock at once | 7.4 | y | y | - | yes |
| 57 | a `FINAL` that fails to decode releases **nothing** | 7.4 | - | - | y | yes |
| 58 | `RELEASE` matches on the IP, from any port | 6.3/7.4 | y | y | y | yes |
| 59 | `RELEASE` from a stranger is an ack, not `ERR_BUSY` | 6.3 | - | y (nobody holds it) | y (stranger) | yes |
| 60 | `STREAM_TIMEOUT_MS` with no frames -> `HOLD` | 7.3 | y | y | - | yes |
| 61 | `HOLD_MS` -> `IDLE` | 7.3/7.5 | - | y (compressed) | y (exact) | yes, but 11 s of wall clock |
| 62 | `HOLD_FOREVER` never leaves `HOLD` | 7.5 | - | y | y | yes, but 11 s |
| 63 | `IDENTIFY` is an overlay: state byte changes, frames still land | 7.3 | - | y | - | yes (state byte + counters) |
| 64 | `IDENTIFY 0` stops one in progress | 6.3 | - | y | - | yes |
| 65 | nothing on either port can stop the device (fuzz) | - | - | y | - | yes, but it is a soak |
| 66 | bit-exact pixels for 27 vectors + hand-built payloads | 4 | - | y | y | **no** - needs `--dump-dir` or the camera |
| 67 | the panel model is applied to the panel, not the pixels | - | - | y | - | **no** (card 066 owns this) |
| 68 | a sender can tell network loss from a slow device | 6.9 | - | y (fault injection) | - | **no** - needs injected faults |

**Nowhere today, and worth adding:** 12 (the `HAS_TS` happy path over the
wire), 21 (a real payload per codec, asserted), 32, 33, 34, 37's bound, 39, 40,
41's missing four, 49, 50, 51/52, 57, 59, 61/62, 63/64. That is the gap this
card closes inside the probe.

**Not portable to the wire, stays in `crates/sim`:** 25, 66, 67, 68, plus all of
20's fault injection and everything in `core_rules.rs`, which is the exact-timing
and different-source-IP half that loopback cannot reach.

**Two things the wire cannot answer at all**, found while writing this table:

- There is no way to *read* the current idle mode. `SET_IDLE` echoes the mode
  it set, telemetry has no field for it and the TXT record has no key. A suite
  that changes it therefore cannot put it back to what it found. Card 131.
- `frames_rejected` cannot distinguish "the device rejected it" from "the air
  ate it", so every negative framing rule has to be retried before it is called
  a failure (the existing `conformance` already learned this the hard way - see
  its comment about two runs in three).

### 2. Where the shared sender goes

`crates/sim/tests/common/mod.rs` holds three things: a `Sender`/`Ctrl` pair of
UDP helpers, four encoders written from section 4's prose, and the vector
loader. The probe holds near-duplicates of the first and third
(`src/link.rs`, `src/vectors.rs`).

Decision: **`crates/probe` becomes a lib + bin**, and the sim's tests
dev-depend on it. Reasons:

- The roadmap already says to grow the suite from `screeny-probe`, and the
  suite needs the encoders (to send a real payload per codec) and the sender.
  Putting them anywhere else means the probe depends on a third crate that
  exists only to feed it.
- A new `crates/wire-test` would be a fourth place wire code lives, which is
  what the ground rule forbids.
- The encoders must **not** go to `crates/encode`. That crate is the product
  encoder; `common/mod.rs`'s encoders are a deliberately independent second
  reading of section 4, and a test that round-trips through the product encoder
  and the product decoder grades `proto` against itself. The comment at the top
  of `common/mod.rs` says exactly this. They move to `screeny_probe::enc` and
  keep the property.
- `screeny-sim`'s *dev*-dependencies gaining `screeny-probe` is not a cycle:
  the probe library depends on `screeny-proto` alone.

`Sender` and `Control` are unified rather than copied: one `FrameLink` that can
do both the probe's `connect` + auto-seq style and the tests' explicit-seq,
explicit-`req_id`, no-retry style.

### 3. The suite, built (2026-09-19)

`crates/probe` is now a lib + bin. New: `src/lib.rs`, `src/enc.rs` (the
encoders, moved), `src/suite/{mod,framing,sequence,codecs,control,telemetry,
arbitration}.rs`. Changed: `src/link.rs` (one `FrameLink`/`Control` with both
API styles), `src/vectors.rs` (loads the `.rgb` expectations too, so there is
one vector loader), `src/main.rs` (the two hand-rolled command bodies deleted;
`conformance` now runs the rule catalogue and `lock-test` is an alias for
`conformance --only 7`).

**64 rules.** `--list` prints them. The runner:

- refuses to start if the device does not answer a `TELEMETRY` request, so
  "the device is not there" is one error and not sixty failures;
- prints the brightness it found and an estimated run time before it starts;
- one line per rule: `[n/64] SECTION  name  PASS|FAIL|SKIP  measurement`;
- skips, with the reason printed, the three rules that cannot be honest
  against a real device (`LOOPBACK_ONLY`: a datagram over 1472 bytes is
  fragmented away by the radio, so "the counter did not move" would look like
  a pass), the two that wait out `HOLD_MS` (`--slow`) and the one that would
  drive the panel to the firmware's brightness cap (`--cap-probe`);
- re-runs a failing rule once when it is flagged `RETRY`, because these count
  exact numbers of datagrams and on WiFi a lost probe is not a lost MUST;
- restores brightness, the idle mode and the lock on *every* exit path -
  `Drop` for a normal return or a panic, a `ctrlc` handler for ctrl-c - and
  prints what it restored to, read back from the device.

Safety, which is the whole reason the card exists: no `SET_WIFI` and no valid
`REBOOT` is ever sent at a device (the `SET_WIFI` *error* cases are real rules
but are `LOOPBACK_ONLY`); `SET_BRIGHTNESS` only ever steps **down** from what
it found and puts it back; `SET_NAME` is exercised by setting the device's
name to the name it already has; `IDENTIFY` is always stopped explicitly; and
every payload is a dim `SOLID` or a checked-in vector, nothing near white.

First run against `screeny-sim` on loopback: **48 passed, 3 skipped, 14
failed** - and all fourteen failures were `Connection refused` because the
simulator I had started with `--exit-after 180` exited underneath the run at
rule 50. The suite reported that honestly rather than passing, which is the
right failure mode. Re-running with a longer-lived simulator next.

### 4. Evidence against the simulator (2026-09-19)

All against `screeny-sim --headless --bind 127.0.0.1 --no-mdns`, every
instance with a bounded `--exit-after`, none left running.

| What | Result |
|---|---|
| `conformance --slow --cap-probe` | **64 passed, 0 failed, 0 skipped**, 89 s |
| `conformance` (the default) | **61 passed, 0 failed, 3 skipped**, 37 s |
| exit code, clean run | 0 |
| exit code, rules failing (against a `--drop 100` simulator, `--only 3.2`) | 1, and each rule shows both attempts |
| ctrl-c 4 s into a run | exit 130, `interrupted: restoring the device / brightness 200 restored` |
| in process, `cargo test -p screeny-sim --test conformance` | ok, 36.5 s |

**Restore, shown rather than asserted.** A simulator started at brightness
137 with `--idle dim`, run with `--verbose`, logged exactly this over one
default run: `brightness 137 -> 137` (the opcode sweep), `brightness 68 -> 68`
(the step-down rule), `brightness 137 -> 137` (its own restore), `idle mode
3, 2, 1, 0` (the `SET_IDLE` rule), then `brightness 137 -> 137` and `idle mode
0` from the suite's exit restore. The run printed `restored: brightness 137
(found 137), idle mode 0, lock released, state HOLD`, and a fresh `stats`
afterwards read 137. `--restore-idle 2` puts it back to 2 instead, confirmed
the same way.

Note what that last line also demonstrates: the simulator was started in
`DIM` and was left in `STATUS`, because **nothing on the wire reports the
current idle mode** (card 131). `--restore-idle` is a stated intention, not a
restore, and the suite says so in its output.

### 5. What moved out of `crates/sim/tests`

Criterion: a test is wire-level duplication when *every* assertion it makes is
on bytes the device sends back - reply datagrams, or the telemetry counters,
which `SimHandle::telemetry()` returns verbatim - and so could be made by an
outside client. Anything asserting decoded pixels, `shown`, `active_source`,
the event stream, `render_display()` or injected faults stays.

| File | Before | After | Removed |
|---|---|---|---|
| `control.rs` | 10 | 5 | the `ERR_UNKNOWN_OP`/`ERR_BAD_LENGTH`/`ERR_BAD_ARG` tables, the malformed-header errors, the `GET_INFO` rate limit, the `REPLY` bit, `FRAME` on the control port, `CONTROL` on the frame port |
| `telemetry.rs` | 5 | 2 | `STATS_REQ` on the frame port, the 100 ms limit, `RESET_STATS` zeroing |
| `sequence.rs` | 6 | 4 | `seq_gaps` arithmetic, the interarrival EWMA |
| `arbitration.rs` | 11 | 9 | `IDENTIFY 0`, and the spec-constants test |
| `malformed.rs` | 5 | 5 | - (it asserts the `Reject` *reason* per case, which the wire cannot distinguish - see card 130) |
| `codecs.rs` | 6 | 6 | - (bit-exact pixels; not observable from outside) |
| `core_rules.rs` | 13 | 13 | - (virtual clock: `LOCK_MS` to the microsecond, two senders on different IPs) |
| `faults.rs`, `cli.rs` | 11 | 11 | - |

**Eleven removed, one added back, one added new.** Added back:
`the_reboot_magic_that_does_work_is_accepted` - the suite checks the *guard*
(`REBOOT` without the magic word is `ERR_BAD_ARG`, safe to ask a panel) and
the word that works can only be sent to something that will not act on it.
`a_request_id_of_zero_means_no_reply_wanted` kept only its in-process half and
is renamed `a_request_with_no_reply_wanted_is_still_carried_out`. Added new:
`tests/conformance.rs`, which runs the whole catalogue in process.

Nothing was lost in the move. The one case that would have been -
`SET_WIFI` cut short earning `ERR_BAD_LENGTH` - is now folded into the suite's
`LOOPBACK_ONLY` `SET_WIFI` rule, alongside its two `ERR_BAD_ARG` cases.

Deleting `the_spec_s_own_constants_behave_the_same_way` deserves a word,
because it was load-bearing: it existed so that `arbitration.rs`'s compressed
timings were not the only evidence. `tests/conformance.rs` now drives the
whole of section 7 at `LOCK_MS` 500 and `STREAM_TIMEOUT_MS` 1000 through the
same rules the bench uses, which is a strictly better version of that
argument.

### 6. Coverage, after

Every row the table in section 1 marked "nowhere today, and worth adding" is
now a rule: 12, 21, 32, 33, 34, 37's bound, 39, 40, 41's missing four, 49, 50,
51/52, 57, 59, 61/62, 63/64. Counting by spec section, as
`conformance --list` prints them: **1 -> 1 rule, 2.x -> 10, 3.x -> 11,
4.x -> 6, 5.x -> 2, 6.x -> 22, 7.x -> 11, 8.x -> 1** - 64 in all, of which 3
are loopback-only, 2 need `--slow` and 1 needs `--cap-probe`.

Rows 25, 66, 67 and 68 remain out of reach of any wire protocol - the pixels
on the panel, the panel model, and telling network loss from a slow device
without injecting the faults yourself - and stay in `crates/sim`, which is
what the card said they would.

### 7. Root test run

`cargo test --release --no-fail-fast` - green, 40 test binaries, 0 failures.
The `crates/screeny` pacing tests (card 093) passed first time under a loaded
host. `cargo clippy -p screeny-probe --all-targets` is clean apart from two
`is_multiple_of` suggestions in code this card did not touch.

### 8. Out of scope, carded

- **130** telemetry cannot say *why* a datagram was rejected: one counter for
  six faults, so a device-side framing rule can only assert "something was
  rejected".
- **131** the idle mode is write-only over the wire.
- **132** sections 1 and 2.3's oversize rules cannot be observed over WiFi -
  the radio fragments the datagram away and the device counts nothing, which
  is exactly what a violation looks like. The spec should say so.
- **133** `screeny-probe --name` resolves a host name where `crates/screeny`
  browses for a DNS-SD instance; they stop agreeing after a `SET_NAME`.

134 is left unused. **No spec/implementation disagreement was found**: all 64
rules pass against `screeny-sim`, so `crates/receiver` and `crates/sim` agree
with `protocol-v1.md` on every MUST this suite can express. Whether the
firmware does is the orchestrator's run.

### 9. For the orchestrator

```
cargo run --release -p screeny-probe -- --addr 192.168.7.221 conformance --slow
```

About 70 s (45 s without `--slow`). Exit 0 or non-zero, one line per rule.
Expect **3 SKIP** lines with `loopback only` as the reason - those are card
132's two oversize rules plus `len > 1464`, which no real link can deliver -
and `--cap-probe` left off, because it would drive the panel to the firmware's
brightness cap, which may be brighter than what the suite found. The last two
lines say what it restored and the tally. `--restore-idle N` if the panel
should be left in an idle mode other than 0 `STATUS`.

### Orchestrator: merged, and run against the real device (2026-09-19)

Merged to `main` cleanly. Root `cargo test --release --no-fail-fast`: 299 passed, 0 failed
(the in-process `tests/conformance.rs` stays on: 36 s is a fair price for a suite that
cannot rot).

`screeny-probe --addr 192.168.7.221 conformance --slow` against the firmware built from
`main` (0.2.0): **60 passed, 0 failed, 4 skipped**, exit 0. Skips: three "loopback only: the
radio fragments it away" (card 132) and the brightness-cap rule that needs `--cap-probe`
(deliberately not run: the panel is on laptop USB). Last line: `restored: brightness 96
(found 96), idle mode 0, lock released, state IDLE`. So the firmware - the third
implementation of the spec - agrees with the simulator on every MUST the wire can express.
