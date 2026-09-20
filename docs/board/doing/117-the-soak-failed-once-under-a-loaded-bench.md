---
id: 117
title: The soak failed once on a loaded bench, and said nothing useful about why
type: test
hardware: no
depends: []
owner: worker-118-117
branch: card/118-117-poll-backoff-and-flakes
---

## Goal

Find out whether `crates/studio/tests/soak.rs::the_server_survives_a_bounded_soak`
has a timing assumption that a loaded machine can break - and, either way, make
its failure say which property went.

## Context

Seen once, on 2026-09-20, by worker-196: a root `cargo test --release
--no-fail-fast` reported

```
test the_server_survives_a_bounded_soak ... FAILED
test result: FAILED. 0 passed; 1 failed; ... finished in 41.16s
```

while another worker's `cargo clippy --workspace` was saturating the machine.
**41 s** is short for a soak that normally takes 69 s, so it gave up early
rather than running out of time. It has not reproduced since:

- alone, `cargo test --release -p screeny-studio --test soak`: passed (69.0 s);
- deliberately under load, with `cargo build --release --workspace` running into
  a scratch target directory (load average 10-20): passed (68.8 s);
- the clean root run that followed: 720 passed, 0 failed.

Card 196's change cannot be the cause - the soak opens no WebSocket, and card
196 only touches what an open socket is sent - but that is an argument, not
evidence, which is the other reason to look.

The soak's assertions that a slow machine could plausibly break are the ones
about *rates and recency* rather than about facts:
`telemetry_ago < 10.0`, `ticks > base_ticks + 100`, `faults >= 3`, and the
`/healthz` 200 inside each round.

## Deliverables

- Whichever it is: a bound widened with a reason beside it, or a real fault
  found and fixed.
- A failure message on every one of those assertions that says the measured
  number, so the next sighting is one line rather than an investigation.
  (`assert!(x > y)` with no message is what made this card necessary.)

## Acceptance

The soak is run enough times under a deliberate load to say something honest
about its flake rate, and a failure now names the property and the number.

## Log

### 2026-09-20, worker-118-117: a cause in the soak, and a bounded re-bind

**The soak: one real cause, and numbers on every wait that could break.**

`recovered()` waited for `panel.frames_sent > before + 10`. That counter is the
**link's** lifetime count, and the supervisor builds a *new* link whenever the way to
reach the panel changes - which round 3, "the panel moves", does twice: once when the
typed address is added (`Reach::Addr`) and again when the telemetry poll resolves it
(`Reach::Resolved`). A new link starts at zero, so if `before` was read before a rebuild,
the wait was not "frames are flowing" at all: it was "the new link has counted its way
past the old link's lifetime total", which at 30 fps is a second per thirty frames. On a
loaded machine the supervisor's re-aim lags, which is exactly when `before` is read too
early and the total is large - a wait that is 0.3 s on an idle bench and tens of seconds
on a busy one, ending in `timed out waiting for frames to flow again`, with no number
beside it. That is the shape of the failure this card is about, and it is now fixed:
`flowing()` counts from wherever the counter is and **re-bases whenever it drops**.

Measured, and it is not hypothetical: a 45 s soak prints

```
soak: after the panel moving to another address the panel's frame counter went backwards,
      6 -> 0: the link was rebuilt, so frames flowing is counted from here.
soak: 45 s, 9 faults in 9 rounds, 2 link rebuilds under the frame counter
```

and a 60 s one, 3 rebuilds in 14 rounds - one per "the panel moves" round, every time.
The count is in the summary line from now on, so a soak that did *not* exercise a link
rebuild says so.

Everything else in the soak now says what it measured when it fails:

- every wait is `until_json` on `/api/v1/status`, whose timeout message carries the whole
  device (the four `until` calls did not);
- `flowing()` names the frame count, where it was counting from, how many rebuilds it saw
  and how long it waited;
- the `/healthz` assertions name the round, the seconds in, and the body;
- the final assertions - `ticks > base_ticks + 100`, `telemetry_ago < 10.0`,
  `state.writes > 0`, `faults >= 3`, the preview and the two health counters - each print
  their own number and what it is being compared with. No bound was widened: none of them
  is a rate, they are "did this thread run at all" thresholds a hundredth of the size of
  what a working studio does, and `telemetry_ago`'s ten seconds is fifty poll periods.
  The message now says so.
- `sim_again()` for round 2's "the panel comes back on the same address": a bounded wait
  for the pair rather than `expect` on the first try, naming the port.

**`crates/screeny/tests/embed.rs`.** `sim_pair` and the re-bind in
`an_attached_link_reconnects_to_the_same_two_ports` both went through
`SimDevice::start(...).expect("sim starts")` on a **named** port pair - the pair a
simulator had just released. New `sim_at(frame, control)`: retry for up to 5 s, 50 ms
apart, and a message that names both ports and the last error. Nothing else changed;
`sim_anywhere` already retried across pairs.

Neither failure reproduced alone, and I did not spend long trying to: the soak's cause
above is shown by measurement rather than by a reproduction, and the embed one is a
port race whose fix does not depend on having seen it.

### 2026-09-20, later: the new message caught a second one on its second run

Repeating the soak to prove it, run 1 of 5 failed - and this time it said what happened,
which is the whole point of the card:

```
the telemetry poll stopped: the last telemetry is 10.0 s old after 60 s, and the poll
period is 0.2 s: {... "last_error":"asking for telemetry: Connection refused (os error
61)", "last_seen_ago":10.0, "player":{"panel":{"connected":true, ...
```

One investigation, one line, exactly as the card asked. **The last round before the
deadline had taken the panel away**: round 2 unplugs it for seven seconds, and if the
soak's clock runs out in the round after that, the final snapshot is read while the
telemetry poll is still working its way back through its own backoff. `telemetry_ago`
was 10.0 s against a bound of 10.0, with the panel connected and frames flowing beside
it. Nothing had stopped; the test was asking "is the poll current *this instant*" when
the property it means is "is the poll alive".

So the end of the soak now **waits, bounded, for the poll to catch up** and asserts on
the answer that satisfied it - the same pattern as every other wait here, and the same
"one moment, one read" rule `until_json` exists for. The 10 s bound is unchanged and now
lives in a named `FRESH` with its arithmetic beside it (fifty poll periods). A poll that
really had died still fails, 30 s later, with the device beside it.

Two flakes, then, in the one test, both of the same family: a test asserting on an
instant that the code had not promised anything about.

### 2026-09-20, third of the family: `crates/studio/tests/fleet.rs`

Sent by the orchestrator while this branch was being proved: `the_page_and_the_panel_are_one`
failed once, at what was line 185, under a full loaded `cargo test` (main has two more
busy host tests since 1678e0a), and passed alone and on the two full runs after it.

The line was

```rust
// And it is still streaming it, not just configured to.
assert_eq!(device_of(&s)["player"]["panel"]["connected"], true);
```

- no message, and worse, **no wait**: `s` is the answer that satisfied the wait for the
*parameter*, and nothing in that wait says anything about the link. The supervisor
rebuilds the link when the studio learns where the panel really is - a typed address
becoming a resolved device - and for the moment in between, `connected` is false. On an
idle machine that rebuild is long over by here; under load it can land exactly here.

Same treatment, and it costs nothing: the parameter wait now waits for **both** halves in
one condition, so the two facts still come out of one read (card 176's rule), the
patience is the file's generous 30 s rather than 5, and the assertion prints the panel's
own account of itself when it fails. Not a reproduction, and not called a fixed cause:
what can be shown is that the test was asserting on an instant nothing had promised.

`crates/studio/tests/fleet.rs`, 12 tests, passes; run 10 times in a row below.

### 2026-09-20: what was run, and what it said

Alone, in a row, `--release`:

- `soak`: **10/10** after the telemetry-catch-up fix, 60-67 s each. (The run before it was
  9/10 - that failure is the second entry above, and it is the reason the count restarted.)
- `fleet` (studio): **10/10**, 10.2 s each.
- `embed` (screeny): **10/10**, 1.0 s each.
- `device_status` (studio): **10/10**, 4-5 s each.
- `screeny-studio --lib`: 57 passed (the new `uptime_going_backwards...` among them).

Under deliberate load - a cold `cargo build --release --workspace` into a scratch target
directory, load average 7-11 on this machine, plus **two copies of the same test binary at
once**, which is the parallel-worktree case the embed flake came from:

- two copies of `embed` simultaneously: 19 passed, twice, 1.0 s;
- two copies of `device_status` simultaneously: 7 passed, twice, 5.1 s;
- `soak` and `fleet` side by side: soak 60 s / 14 rounds / 1 rebuild, fleet 12 passed;
- `soak`, `device_status` and `embed` side by side under a second cold build: all passed,
  and card 118's two measurements held at 2.0 s and 1.1 s.

The whole suite, at the root: `cargo test --release --no-fail-fast` -> **744 passed, 0
failed** (exit 0). `cargo clippy --workspace --all-targets` says nothing.

The scratch target directories (820 MB and 827 MB) were deleted afterwards, and `ps` shows
no simulator, studio or cargo process left behind.

**Two new cards**, both found here and neither done: **143** (`crates/sim`'s
`a_sender_can_tell_network_loss_from_a_slow_device`, the third sighting - it asserts
`frames_rx == sent` exactly after a fixed 200 ms settle against a deliberately slow
device; left alone because another worker owns `crates/sim`) and **144** (`frames_sent` is
the link's count and restarts when the studio rebuilds the link, which is the *cause* of
the first soak flake and is visible on the page).

### Note from the orchestrator (2026-09-20)

A second one of the same family, seen once while merging 196 with two other worktrees
running the suite: `crates/screeny/tests/embed.rs::reconnection_can_be_turned_off` panicked
in `sim_pair` ("sim starts"). Those tests stop a simulator and start another on the *same*
port pair; a parallel run of the same suite in another worktree can take the port in
between. 19/19 three times when run alone. Worth the same treatment here: a retry with a
bounded wait on the re-bind, and a message on the `expect` that says which port.
