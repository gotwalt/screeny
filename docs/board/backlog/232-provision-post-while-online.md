---
id: 232
title: Credentials posted while Online or Joining - the state machine handles them; small API-crate refinements from the first consumer
type: build
hardware: no
depends: [221, 224, 226]
owner:
branch:
---

## Goal

Card 224 (the simulator) was the first real consumer of `crates/provision` and
`crates/device-api` and found one real gap and a few paper cuts. Close them before the
firmware's portal card (223) builds on the same crates.

## Context

Read first: `CLAUDE.md`; `docs/design/device-web.md`; the "Crate feedback" section of
`docs/board/done/224-sim-http-and-wifi-states.md`; `crates/provision/src/machine.rs`,
its README and `tests/transitions.rs`; `crates/device-api/src/route.rs`;
`crates/sim/src/api.rs` and `src/http.rs` (the consumer you will simplify); spec section
8.2; `firmware/src/main.rs`'s `SET_WIFI` handling (card 212: what the device really does
today when new credentials arrive while it is online).

**The gap.** `Event::CredentialsPosted` is honoured only in `Portal` and `Trial`; in
`Online`, `Joining` and `Boot` it falls through and is dropped. But spec 8.2 says
`SET_WIFI` works from any state, and card 223's LAN settings page posts credentials
while `Online`. The simulator currently answers `503 unavailable` over HTTP for that
case, deliberately and loudly.

**Decided semantics** (they match what firmware 0.3.0 does for `SET_WIFI`):

- `Online` + `CredentialsPosted` -> `Trial`, **without raising the AP** (the device has
  one station: it must drop the current association to try the new network, and there is
  no phone on a setup network to keep informed). Actions: `StopJoin` if needed, then
  `StartJoin { which: Trial, attempt: 1 }`. Same attempt rules as a portal trial (an
  authentication error fails at once; other reasons retry up to 3).
- That trial succeeding: `CommitCredentials { Trial }`, `Announce`, `Online`. No AP was
  up, so no 30 s grace and no `DropAp`.
- That trial failing: **back to the previous network, not to the portal**:
  `StartJoin { which: Stored, .. }` and state `Joining`, with the failed trial's outcome
  kept for `trial()` / `GET /api/v1/wifi` and `wifi_state()` reading `FAILED` until the
  next post or reboot (sticky, as firmware 0.3.0 does - otherwise the old network
  reconnecting a few seconds later reads as "your new network worked"). If the stored
  credentials then also fail 3 times, the ordinary `Joining` -> `Portal` rule applies.
- `Joining` + `CredentialsPosted`: same as `Online` (cancel the current attempt first).
  `Boot` + `CredentialsPosted`: cannot happen before `Event::Boot` is processed; keep
  ignoring it, and say so in a comment and a test.
- A trial that began from `Portal` behaves exactly as today. The machine must remember
  where a trial came from; add that to `Trial` state or a field, your call.
- The telemetry overlay: `PROVISIONING` is shown for portal-origin trials (as today);
  for an online-origin trial do **not** set the overlay (frames may still be arriving
  around the rejoin and nothing about the panel is "in setup").

**The paper cuts** (all in `crates/device-api`, additive only):

- `route::SCAN_MIN_INTERVAL_MS: u32 = 10_000` with the doc comment's "one scan per 10 s"
  pointing at it; and a tiny `no_std`, wrapping-safe `RateLimit { .. }` taking `now_ms`
  (a refused request does **not** reset the timer - same answer card 221 gave for the
  portal retry), with tests including the u32 wrap.
- `route::find(path, method) -> Option<&'static Route>`, plus a way to tell "path known,
  wrong method" (405) from "unknown path" (404).
- `FirmwareReply`: leave the shape alone; in the docs say `ok: true` means *every check
  the implementation claims* passed and that a build which cannot run all five checks
  must answer with the crate's not-implemented/unavailable error instead. Do not add
  fields.

Then make `crates/sim` use all of it: the 503 for a post while `Online` goes away (it
now runs a real online-origin trial through the machine, over HTTP **and** its `wifi_*`
handle methods; UDP `SET_WIFI` behaviour on the wire stays byte-for-byte what it is -
the 64-rule conformance test is the gate), the local `SCAN_MIN_INTERVAL_MS` and the
hand-written route lookup are replaced by the crate's.

## Deliverables

1. `crates/provision`: the transitions above, README diagram/table updated, new rows in
   `tests/transitions.rs` (every new transition; sticky `FAILED`; stored credentials
   untouched by a failed online-origin trial; a second post during an online-origin
   trial; `ButtonWipe` during one; the u32 time wrap across one).
2. `crates/device-api`: the constant, `RateLimit`, `route::find`, the doc note; tests.
3. `crates/sim`: uses them; its HTTP test for "post while online" now asserts the trial
   and the fallback instead of the 503.

## Out of scope

`firmware/` (a hardware worker, card 222, owns it right now), `crates/proto`,
`crates/receiver`, `crates/settings`, `crates/probe`, `crates/studio`, the spec,
`docs/design/*`. `Host::wifi`'s `&'static str` (card 224's proposal 234) is shared
surface and is not yours.

## Acceptance

`cargo test -p screeny-provision -p screeny-device-api -p screeny-sim` green, including
the simulator's 64-rule conformance test unchanged; clippy clean on the three crates;
both `no_std` crates still check for `thumbv7em-none-eabi`; root `cargo test` green
apart from the known-flaky `crates/studio/tests/fleet.rs`
`a_typed_address_becomes_a_device_and_starts_playing` (the software session's; if it
fails, re-run once and report, do not touch it).

## Log
