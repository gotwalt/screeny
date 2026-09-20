---
id: 118
title: One refused connection sends a known panel to the slowest HTTP poll
type: build
hardware: no
depends: [180]
owner: worker-118-117
branch: card/118-117-poll-backoff-and-flakes
---

## Goal

A panel that has served its status API before should not be treated as "has no HTTP
API" because one connection was refused while it was booting.

## Context

`crates/studio/src/fleet.rs`, the device HTTP poller: a fault with `absent: true`
(connection refused, or something that is not this API) puts the device straight at
`MAX_BACKOFF` - 12 polls, about two minutes - "since a firmware update is the only thing
that changes the answer". That reasoning holds for firmware 0.2.0, which has no server.
It does not hold for a panel that answered a minute ago: a rebooting panel has its
network stack up before its HTTP workers listen, and refuses for a moment.

Seen on 2026-09-20 while the firmware session reflashed the bench panel several times:
frames and UDP telemetry were back within seconds of each boot, and the page's Device
block (facts, health flags, firmware version) stayed up to two minutes behind each time -
it showed `0.5.0` for 110 s after `0.5.1` was streaming. With OTA coming (cards 240-243) a
reboot is a thing the Studio will cause and then want to see the result of.

## Deliverables

- In the poller: `absent` jumps to `MAX_BACKOFF` only for a device that has never been
  read (`http.reads == 0`); one that has climbs the ordinary `fail` ladder from the
  bottom. Perhaps also: a telemetry `uptime_s` that went backwards (the panel rebooted)
  clears that device's HTTP backoff.
- Still one poller, one connection in flight, never faster than
  `MIN_DEVICE_HTTP_EVERY`; still logged per transition, not per attempt.
- A test in `crates/studio/tests/device_status.rs`: a simulator whose HTTP comes up a few
  seconds after its UDP is read within a couple of poll periods, not after two minutes.

## Acceptance

Reboot the bench panel (`screeny reboot`, or the page's button): the Device block shows
the new boot within about twenty seconds of the picture coming back.

## Log

### 2026-09-20, worker-118-117: the two rules, and what they measure

**The rules, both in the poller's hands.**

1. `crates/studio/src/fleet.rs`, `status_once`: `absent` is now a claim the *caller*
   makes - `let absent = fault.absent && record.http.reads == 0` - and it is that value,
   not `fault.absent`, that decides the log line, what the registry records and whether
   the device jumps to `MAX_BACKOFF`. A panel that has answered once has disproved "this
   firmware serves no such API", so a refused connection from it is an ordinary failure
   and climbs the `fail` ladder from the bottom (1, 2, 4, 8, 12 passes).
2. `crates/studio/src/devices.rs`, `Registry::heard`: telemetry whose `uptime_s` is
   *smaller* than the last one is the panel having rebooted, and sets a per-device
   `http.retry_now` flag. `status_once` takes it (`Registry::take_http_retry`) at the top
   of each device's turn and drops that device's backoff entry. It does not poll faster -
   the tick is still `device_http_every`, one connection in flight, one poller - it only
   forgives the skips.

Everything card 180's Log asked for still holds: one task spawned once, each read
awaited before the next is started, `MIN_DEVICE_HTTP_EVERY` untouched, `spawn_blocking`,
logged per transition. The only new log line is the one that was already there, now said
for the right devices.

**Measured**, `cargo test --release -p screeny-studio --test device_status -- --nocapture`,
at a 1 s poll period (chosen so that "the cap" - 12 polls - is far enough from "a poll or
two" to tell apart on a clock):

| | before (HEAD) | after |
|---|---|---|
| status API comes back on its own port, panel never went quiet | 13.0 s (13 polls), and `http.absent` was `true` with `reads: 1` | **2.0 s** (2 polls), `absent` stays `false` |
| the whole panel reboots (UDP and HTTP together, new `boot_id`) | 13.0 s (13 polls) behind the reboot | **1.0 s** (1 poll) |

The "before" numbers are the same two new tests run with `fleet.rs`/`devices.rs` checked
out at HEAD, so they are this code's own measurement of the old behaviour, not a
recollection. At the product's 10 s period the same two counts are ~130 s before and
10-20 s after, which is the card's acceptance.

**Tests.** `crates/studio/tests/device_status.rs`:

- `a_panel_that_has_answered_http_is_not_absent` - the isolated rule. The simulator's UDP
  half never goes quiet (`--no-http` equivalent: `SimConfig { http: false, .. }`); the
  status API is the file's own `CountingServer`, now startable on a *named* port
  (`start_at`), so it can be stopped and put back where it was. Asserts `absent == false`
  while the port is refusing - which is the card's title, and fails outright on the old
  code - that the last facts stay on the page, and that the next read lands within 8 s.
- `a_panel_that_rebooted_is_read_again_within_a_poll_or_two` - the acceptance in a
  simulator: the sim is dropped and restarted on the same ports, and the new `boot_id`
  has to reach the page within the same 8 s.
- `devices.rs`, `uptime_going_backwards_tells_the_status_poller_to_ask_again` - the
  uptime rule alone: the first reading is not a reboot, uptime going up says nothing,
  uptime going down sets the flag exactly once.

The 8 s bound is a *generous* one (card 093): two or three polls is the behaviour, twelve
is the cap, and the bound sits between them with room for a loaded machine to slip ticks.
Every one of these assertions prints the measured seconds and the poll count.

`crates/sim` was not touched: the simulator can already be started with its HTTP off, and
the "HTTP comes up later" half is a hand-rolled acceptor in the test - with
`set_nonblocking(false)` on the accepted socket, which the file already did for the macOS
trap.

**Runs.** `device_status` 10/10 alone (4-5 s), and passing with two copies of the binary
running at once under a cold `cargo build --release` (load average 7-11), where the two
measurements were 2.0 s and 1.1 s - the same as idle, because they are counts of polls
rather than of milliseconds. `cargo test --release --no-fail-fast` at the root: 744
passed, 0 failed. `cargo clippy --workspace --all-targets`: nothing.

**Still open, and only the owner can close it:** the card's acceptance is the *bench*
panel rebooting. No hardware was touched here (this card is `hardware: no`), so what is
proved is the simulator's version of it. The firmware session reflashing the panel is the
next chance to watch the Device block follow a real reboot; at the product's 10 s poll it
should be one or two polls behind the picture coming back, not twelve.
