---
id: 156
title: Four more tests that each failed once under a loaded full suite
type: test
hardware: no
depends: [117]
owner:
branch:
---

## Goal

Card 117's treatment for the rest of the family: make each say its numbers when it fails,
wait on what it asserts, and bound it on the code's own schedule, not the OS scheduler's.

## Context

Reported by the firmware session on 2026-09-20: under a loaded full `cargo test` (two
Claude sessions and their workers build and test on this one Mac all day), each of these
failed once and passed alone:

- `screeny-studio` `tests/moved.rs` (card 141's two-simulator test; it re-binds ports)
- `screeny-studio` `tests/soak.rs` - check first whether that run had card 117's fix
  (`sim_again()`, `flowing()`); it may already be dealt with
- `screeny` `tests/pacing.rs::holds_thirty_fps_within_one_percent` - card 093 rebuilt this
  file around the pacer's own wake-ups; a 1% wall-clock bound may still be one too many
- `screeny` `tests/loopback.rs::the_reported_rate_is_the_same_at_any_stream_length`

Nobody captured a failure message. Until this is done the rule in `docs/ORCHESTRATOR.md`
holds: a single failure in one of these under a loaded bench is re-run alone before it is
believed, and both results are reported.

## Deliverables

Each test either shown to have a cause (fixed) or made to print what it measured; 10 runs
alone and one under a cold `cargo build --release` into a scratch target dir.

## Acceptance

Three full-suite runs in a row under load with none of the four failing, or a failure that
says enough to fix.

## Log

## A failure message, captured (card 161's worker, 2026-09-20)

The card says nobody had one. `tests/moved.rs::the_status_poll_follows_a_panel_that_moved`,
caught while three test binaries ran side by side; it passed 3 of 3 alone straight after.
It is not port re-binding. `/api/v1/status` at the timeout said `discovery.probes: 0` and
`last_seen_ago: 0.0`, with the device still on its **old** `control_addr`:

- the probe only runs for a device `Registry::unheard(stale_after)` returns;
- the replacement simulator takes the **same HTTP port**, so the device-HTTP poll (about
  three reads a second here) starts succeeding and refreshes `seen_unix` at once;
- after that the device is never unheard again, so the probe never fires and the test
  waits out its 30 s for a `control_addr` that will not move.

So it is a race between the probe tick and the first successful HTTP read, and under load
the probe loses. Whatever the fix, waiting on `discovery.probes` to advance would at least
make it say which of the two happened.

### A fifth (orchestrator, 2026-09-20)

`crates/screeny/tests/traffic.rs::a_stream_counts_every_datagram_it_sent_and_every_one_that_came_back`,
line 54: `assert_eq!(state.frames.len(), s.frames.out.packets, "the receiver saw every
one")` - an exact count of UDP datagrams across loopback, which a loaded kernel may drop.
Failed once in a full-suite run while a worker was building; 4 of 4 alone, five times.
The sender-side equalities in that test are exact by construction and should stay exact;
the receiver-side one wants a bound, or a retry of the whole stream.
(Card 161's worker also left a diagnosis of `moved` above: a race between the probe tick
and the first HTTP status read refreshing `seen_unix`.)

### `moved` is fixed; more for the list (orchestrator, 2026-09-20 evening)

`moved` was not a load flake: see card 178's closing note - `Registry::unheard` now reads
`udp_seen_unix`, which a status read does not refresh. 12 of 12 since. Still open, from
card 178's worker: `studio/tests/api.rs::a_watching_browser_gets_the_full_rate` and
`the_socket_delivers_frames` assert `n > 18` frames and got 17 under load;
`screeny/tests/indexed.rs::exactness_holds_over_a_stream` ("all thirty displayed") is an
exact loopback UDP count like `traffic.rs`'s; and the Studio's 5 s render watchdog is
wall-clock, so an oversubscribed bench can make it refuse a patch that was merely
descheduled (it took `soak` and `moved` down when the fixture was `metaballs`; they are on
the cheaper `flock` now, which dodges it rather than fixing it).

### Parked 2026-09-21 (owner: "I don't need to over-build this"; the three that matter are 188, 187, 199)

The standing rule covers it: one failure under a loaded bench is re-run alone, both results reported, and a merge is not held for it (docs/ORCHESTRATOR.md, Merging). Reopen if one of the four starts failing alone.
