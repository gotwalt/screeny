---
id: 180
title: Studio reads the device's own HTTP status, when the firmware serves it
type: build
hardware: no
depends: [170, 222]
status: parked
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
  **Parked until 222 and 224 have merged** - check with the firmware session first.
- Card 106 left exactly one seam for this: `crates/studio/src/fleet.rs`, the telemetry poll
  ("the single place the studio asks a device about itself"). Card 170 may have moved it;
  find where it lives now. The UDP control-port telemetry stays the fallback for firmware
  that has no HTTP server, and stays the source for frame counters.

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
