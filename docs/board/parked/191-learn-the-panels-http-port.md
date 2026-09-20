---
id: 191
title: Learn a panel's HTTP port from mDNS instead of assuming 80
type: build
hardware: no
depends: [180]
owner:
branch:
---

## Goal

The Studio finds a panel's HTTP API at *the resolved frame address, on port 80*. The
device advertises `_http._tcp` under the same instance name. Read the port from that
advertisement rather than assuming it.

## Context

- Card 180 derives it: `DeviceRecord::http_addr(default_port)` in
  `crates/studio/src/devices.rs` takes the resolved `frame` address's IP and a port -
  the device's own override, else the studio-wide `--device-http-port`, else 80.
- Assuming 80 is right for every panel that exists today, and the override exists
  because a simulator cannot bind 80 without root. So this is not a bug; it is an
  assumption that is currently true and is written down in one place.
- `docs/design/device-web.md` says the device advertises **`_http._tcp`**. The Studio
  browses `_screeny._udp` only (`crates/studio/src/devices.rs::browse` ->
  `screeny::discover::browse`). Reading the second service type would mean a change in
  `crates/screeny`, which is why this is its own card rather than part of 180.
- The case it would actually fix: a panel behind something that maps its HTTP somewhere
  else, or a second simulator on the same host. Neither exists yet. Worth doing when
  discovery is being touched anyway, not before.

## Deliverables

- `screeny::discover` able to browse `_http._tcp` and match an instance to a
  `_screeny._udp` one by name.
- `DeviceRecord::http_port` filled in from that when it is found, left alone when it is
  not. No state schema change: a port is live data, like an address.

## Acceptance

Two simulators on one host, each with its own `--http-port`, both advertising: the
Studio reads the right status from each, with no `--device-http-port` given.

## Log

### Parked 2026-09-20 (owner: "let's prune what doesn't need to be done"; focus is aesthetic work)

An assumption (HTTP on port 80) that is true for every panel there is, written down in one place. The card itself says: worth doing when discovery is being touched anyway, not before.
