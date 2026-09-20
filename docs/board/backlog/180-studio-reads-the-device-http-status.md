---
id: 180
title: Studio reads the device's own HTTP status, when the firmware serves it
type: build
hardware: no
depends: [170, 222]
owner:
branch:
---

## Goal

The panel section of the Studio page shows what only the device knows - heap, free stack,
WiFi state and SSID, IP, firmware slot and state, reset reason, settings-store errors -
next to what the UDP telemetry already gives it.

## Context

- The firmware session defined the device's HTTP API in `crates/device-api` (package
  `screeny-device-api`): routes, serde types, one error shape, golden JSON for every request
  and reply in `crates/device-api/tests/golden/` (start with `status.json`). Design: "The
  HTTP API" in `docs/design/device-web.md`. The types round-trip through plain `serde_json`,
  so the Studio should **depend on the crate** and not restate the shapes (one
  implementation of each thing).
- The firmware does not serve it yet: card 222 (firmware session) is what makes it real, and
  card 224 adds the same API to `crates/sim`, which is what this card's tests run against.
  Both have merged (2026-09-20).
- Card 106 left exactly one seam for this: `crates/studio/src/fleet.rs`, the telemetry poll
  ("the single place the studio asks a device about itself"). Card 170 may have moved it;
  find where it lives now. The UDP control-port telemetry stays the fallback for firmware
  that has no HTTP server, and stays the source for frame counters.

- 2026-09-20: firmware card 224 has merged - `screeny-sim` serves the HTTP API
  (`--http-port`, `--start-in-portal`, `--no-http`; `SimHandle::http_addr()` / `http_url()`
  in tests; `Config::for_test()` uses an ephemeral port), `boot_id` included. So this card
  can be built against the sim as soon as card 170 has merged; the real device follows with
  firmware card 222.
- 2026-09-20: **unparked.** Firmware 0.4.0 (their card 222) serves the API on the real device,
  port 80, LAN side only: `GET /api/v1/status` (all fields real, `boot_id` included),
  `/telemetry`, `/wifi`, `POST /settings|identify|reboot|wifi`; `/networks` and `POST
  /firmware` answer 503 `unavailable` for now. Constraints from the firmware session, to be
  designed around: the device has ONE connection worker and no listen backlog, so a second
  simultaneous connection is dropped at SYN and retried by the OS a second later; keep-alive is
  off. So: poll `/api/v1/status` **no faster than every 10 s, one connection at a time,
  `Connection: close`, ~2 s timeout, capped backoff**, never from more than one task, and never
  let a browser trigger a poll (browsers read the Studio's cached copy). Measured by them: 200
  requests in 60 s during a 30 fps stream cost no frame.
- `wifi_state` in `/api/v1/status` can read `failed` while the device is online: until the
  firmware session's card 223 it is the sticky result of the last credentials *attempt*, not
  the link. Rule until then: a non-null `ip` means connected; show "last WiFi change failed"
  only as a note, never as a fault. After 223 it means the link (connected / connecting /
  disconnected) and the attempt's outcome lives in `GET /api/v1/wifi`.
- **The status payload contains the real WiFi SSID.** Show it on the owner's page; never write
  it into a tracked file, a fixture, a log line, a card or a screenshot (`CLAUDE.md`). Tests
  use the simulator, whose SSID is a dummy.
- Agreed with the firmware session (2026-09-19): the status payload will carry `boot_id`, a
  random u32 drawn once at boot (no flash wear). Same id = the link flapped; different id =
  the device rebooted. Use that to count reboots; do not infer them from uptime.

## Deliverables

- An HTTP poll of `GET /api/v1/status` on the attached device (bounded: timeout, capped
  backoff, one in flight, never on the render thread), merged with UDP telemetry into the
  status the page shows; absent or failing HTTP is normal and silent after the first note.
- The panel section shows the new fields plainly; a settings-store error or an unexpected
  `reset_reason` (brownout, panic, watchdog) is the one thing that should stand out.
- Tests against `screeny-sim` with the HTTP API on and off.

## Acceptance

With firmware that serves the API, the page shows the device's heap, WiFi and firmware slot;
with firmware 0.2.0 it looks exactly as it does today.

## Log
