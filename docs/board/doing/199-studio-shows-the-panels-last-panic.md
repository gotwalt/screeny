---
id: 199
title: The Panel screen says when the panel last panicked
type: build
hardware: no
depends: [180, 198, 243]
owner: worker (sonnet)
branch: card/199-panel-last-panic
---

## Goal

Firmware 0.5.2 (card 243) leaves a breadcrumb when it panics and serves it at
`GET /api/v1/panic`. A Studio that runs unattended for months is the thing that should
notice: show it on the Panel screen, and let it colour the Picture screen's status chip the
way the other device faults do.

## Context

- The route is **not** part of `GET /api/v1/status` (growing `StatusReply` cost the device
  3.5 KB of stack, so the firmware session moved it): `screeny_device_api` has `PanicReply`
  and `route::PANIC`; the reply is `{"boot_count":u32,"panic_count":u32,"last_panic":null |
  {"uptime_ms","boot","file","line","consecutive"}}`; goldens `panic.json` /
  `panic_none.json`; `screeny-sim` serves it.
- It cannot change while the device runs. **Read it once per `boot_id` change, never on the
  10 s poll** (the firmware session's request): the panel has one connection worker.
  `crates/studio/src/fleet.rs`'s status poller already sees `boot_id` in every status
  reply and already has the one-connection-in-flight rule; this is one more request on that
  same task, after a status read whose `boot_id` is new. Firmware older than 0.5.2 answers
  404: that is "no such route", said nowhere, not a fault.
- Card 195's rule for wording: say what is known ("panicked at panic.rs:42, 3 boots ago"),
  not what it might mean. `consecutive` > 1 is the one that deserves the fault tone.
- The SSID rule does not apply to this payload, but the usual one does: no real device's
  output in a fixture or a Log.

## Deliverables

- `devhttp.rs`: `get_panic`; `fleet.rs`: asked once per new `boot_id`; `devices.rs`: kept
  beside `facts`, in `/api/v1/status` under the device.
- `ui/panel.js` + `panel.html`: a line in the Device block; `common.js`'s `attention`: a
  reason for the chip when `consecutive` > 1.
- Tests against `screeny-sim`: read once per boot and not again; a 404 is silence; the
  connection count test in `tests/device_status.rs` still holds.

## Acceptance

The deliberate-panic build the firmware session keeps for this shows up on `/panel` within
a poll or two of the panel coming back, and never costs the panel a second connection.

## Log

### Note from the orchestrator (2026-09-20, evening)

From the firmware session, with fw 0.7.0 on the panel: there is now an always-on core-0
liveness watchdog (20 s). If the firmware wedges, the panel reboots itself and
`GET /api/v1/panic` says so - the reply has gained `"last_reset":"wdt"` and an `update`
field for OTA outcomes. Read `crates/device-api`'s `PanicReply` and its goldens for the
final shape before building this; a watchdog reset deserves the same line on `/panel` as a
panic. Known firmware bug at the time of writing: a firmware *upload* wedges core 0 (the
watchdog recovers it, nothing is activated); OTA does not work yet, and the firmware
session asked that no Studio upload support be built against it until it says so.

### The final shape (from the firmware session, 2026-09-20 evening; OTA passes on fw 0.7.0)

`GET /api/v1/panic` -> `{"boot_count","panic_count","last_panic":null|{uptime_ms,boot,
file,line,consecutive},"update":null|{"outcome":"trial"|"confirmed"|"reverted","reason":
null|"deadline"|"aborted"|"rejected","slot":"ota_0"|"ota_1","version":"x.y.z"},
"last_reset":"power_on"|"software"|"wdt"|...}`. Types and goldens are in
`crates/device-api`; the spec is `docs/design/protocol-v1.md` 8.6/8.10. Read once per
`boot_id` change, never on the poll. A wedge shows as `last_reset: "wdt"` and a climbing
`boot_count`; an update's outcome belongs on the same line of `/panel` (see card 185).
