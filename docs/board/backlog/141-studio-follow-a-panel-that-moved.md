---
id: 141
title: Follow a panel that has moved, without being told
type: build
hardware: no
depends: [106]
owner:
branch:
---

## Goal

A panel added by **name** follows a DHCP lease, because the link re-resolves the name on
every reconnect. A panel added by **address** does not: when it moves, somebody has to
say where to (`POST /api/v1/devices/add {to, device}`, card 106). That is honest, but it
is not "built to be forgotten", and mDNS is exactly what is missing in the two
deployments that matter least predictably - Docker on macOS, and a Linux box where avahi
owns 5353.

## Context

`screeny::discover::broadcast_probe` (spec 5.5) answers this without multicast: a
`GET_INFO` to the subnet broadcast address, and every panel answers with its `id=`. The
registry already keys by that id, and `Registry::set_address` already moves a device
without disturbing its player, so the machinery is there.

## Deliverables

- A probe, on the same slow schedule as the browse and only for devices that have been
  unheard past `Config::stale_after`: send the broadcast `GET_INFO`, match the answers
  by device id, and update the address of any that have moved.
- Bounded: one probe in flight, capped backoff, logged once. Off by the same
  `--no-discover` flag.
- A test against two simulators on loopback: the one that moved is found at its new
  port and keeps its player, its piece and its seed; the one that did not is untouched.

## Acceptance

Move the bench panel to a new DHCP lease and leave the studio alone: it catches up.

## Log
