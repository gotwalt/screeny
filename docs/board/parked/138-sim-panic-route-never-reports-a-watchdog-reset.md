---
id: 138
title: screeny-sim's GET /api/v1/panic always answers last_reset null, so nothing can test the wdt line against a simulator
type: build
hardware: no
depends: []
owner:
branch:
---

## Goal

`docs/design/protocol-v1.md`'s reply.rs docs say `PanicReply::last_reset` is "the same
value as `StatusReply::reset_reason`" (card 241b). `crates/sim/src/api.rs`'s
`panic_breadcrumb` does not read it from anywhere: it is hard-coded

```rust
// Card 241b: a simulator is a process. If it stops, the operating
// system is what says so, and there is no reset register to read.
last_reset: None,
```

`crates/sim`'s `Health` (`crates/sim/src/config.rs`) already carries a settable
`reset_reason` - `--reset-reason wdt` on the CLI, or `SimHandle::set_health` in a test -
and `GET /api/v1/status`'s `reset_reason` already reads it (`crates/sim/src/api.rs::status`).
`GET /api/v1/panic` is the one place that fact does not reach, so nothing that talks only to
the simulator can ever see `panic.last_reset: "wdt"` - which is exactly the shape card 199's
Studio work (the Panel screen's "the watchdog reset it" line) needed to integration-test and
could not, per its Log.

## Context

Card 199 built and unit-tested the Studio's *wording* for a `wdt` reset with a hand-written
test double (`crates/studio/tests/device_status.rs`'s `PanicServer`), because the real
simulator cannot produce the shape. That test double is fine for what it proves (the HTTP
plumbing, "once per boot"), but nothing exercises the whole path - `screeny-sim` ->
`crates/studio` -> the page - for a watchdog reset the way `with_http_the_page_has_what_only_the_device_knows`
does for the ordinary case.

The comment above `last_reset: None` reads as though it were a deliberate decision ("a
simulator is a process ... there is no reset register to read"), but `reset_reason` is
exactly that same "no real register" case and the simulator already answers it from
`Health` on `GET /api/v1/status`. The two routes disagreeing about a fact the spec says must
be the same value looks like an oversight from before card 199's `last_reset` field existed
on `PanicReply`, not a considered choice.

## Deliverables

- `crates/sim/src/api.rs::panic_breadcrumb`: read `last_reset` from the same `Health` (or
  the same derivation `status()` uses) rather than hard-coding `None`, so the two routes
  agree the way the spec says they must.
- A test that sets `--reset-reason wdt` (or `SimHandle::set_health`) and asserts both
  `GET /api/v1/status`'s `reset_reason` and `GET /api/v1/panic`'s `last_reset` read `"wdt"`.
- Check whether `crates/studio`'s `Registry::heard_panic`/`PanicFacts` (card 199) needs
  anything once this is real - it should not, since it already reads whatever `last_reset`
  the reply carries, but worth a green run of `tests/device_status.rs` after this lands.

## Acceptance

`screeny-sim --reset-reason wdt` answers `"wdt"` on both routes; a studio pointed at it shows
the Panel screen's watchdog line without a hand-written stand-in server.

## Log

### Parked 2026-09-21 (owner: "flag those followups for later... good enough for right now")

Simulator-only: the real device reports `last_reset` correctly, and nothing the owner looks at
reads the sim's `/api/v1/panic`. Reopen when a Studio test needs to play a watchdog reset
through that route. Owner of the crate: the firmware session.
