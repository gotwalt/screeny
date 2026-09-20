---
id: 118
title: One refused connection sends a known panel to the slowest HTTP poll
type: build
hardware: no
depends: [180]
owner:
branch:
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
