---
id: 081
title: sim - model the Wi-Fi join states and the PROVISIONING overlay
type: build
hardware: no
depends: [006]
owner:
branch:
---

## Goal

`screeny-sim` answers `GET_WIFI` with a fixed SSID and `CONNECTED`, and accepts
`SET_WIFI` without acting on it. That is the right call for card 006 - there is
no radio - but it means the two sender-side paths that only exist *because*
Wi-Fi can fail have never been exercised: the `ERR_WIFI` / `FAILED` join state
of spec section 8.2, and the `PROVISIONING` state byte of section 6.7.

## Context

- Spec section 8.2: the device replies to `SET_WIFI` **before** disconnecting,
  then tries up to 3 times, then falls back per 8.3 and sets the join state so
  `GET_WIFI` reports `ERR_WIFI`.
- Spec section 6.7: `state` byte 4 is `PROVISIONING`, an overlay like
  `IDENTIFY`. Nothing in the simulator ever sets it.
- Spec section 7.3: "any | Wi-Fi link down | `HOLD`" - the simulator has no way
  to simulate a link going down, so that transition is untested in two of the
  three implementations.
- Section 8.4's invariant holds throughout: the PSK is never returned, never in
  telemetry, never logged. `crates/sim`'s `Event::SetWifi` has no field for one
  and must not grow one.

## Deliverables

In `crates/sim`:

- A scripted join outcome: `--wifi-result ok|fail|slow`, or a
  `SimHandle::set_wifi_outcome(..)` so a test can choose per request.
- `SET_WIFI` then drives the real sequence: reply first, go `PROVISIONING`,
  spend a configurable time, then land on `CONNECTED` or `FAILED`, with
  `GET_WIFI` and the telemetry state byte following along.
- `SimHandle::set_link_down(bool)`, which takes the section 7.3 transition to
  `HOLD` and, after `HOLD_MS`, to an idle screen that says the network is down.
- Tests for all of it, and the "network is down" idle screen drawn in
  `screens.rs` next to the status screen.

## Acceptance

`cargo test -p screeny-sim` covers a failed join end to end, and a sender can
be developed against a device that sometimes cannot get on the network.

## Log

2026-09-20, firmware orchestrator: absorbed into card 224 (the simulator serves the
device's HTTP API and models the WiFi states through `crates/provision`). Closed here;
the work and its log are on 224.
