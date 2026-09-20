---
id: 141
title: Follow a panel that has moved, without being told
type: build
hardware: no
depends: [106]
owner: worker (Claude Opus 5)
branch: card/141-follow-a-moved-panel
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

### Worker, step 1 (claim)

Claimed on `card/141-follow-a-moved-panel`. Read `CLAUDE.md`, `docs/README.md`,
card 176's log (the named browse must stay at ~0.07 s - nothing here touches
`browse`/`browse_for_name`), `crates/screeny/src/discover.rs`,
`crates/studio/src/{devices,fleet,lib}.rs` and the existing two-simulator tests
(`crates/studio/tests/fleet.rs`, `tests/device_status.rs`).

**The plan, decided before writing anything.**

- `discover.rs`: split `broadcast_probe` into `broadcast_targets(port)` (the
  subnet broadcast address of every usable IPv4 interface) and
  `probe(timeout, &[SocketAddr])` (send `GET_INFO` to each, collect the unicast
  replies). `broadcast_probe` stays, as the two composed, so the CLI's
  `--broadcast` is untouched. Injectable destinations are what a loopback test
  needs and what a container with no broadcast reachability needs.
- Studio: one new `Config` field, `probe_to: Vec<SocketAddr>` (empty = the
  subnet broadcast). The probe runs on the browse's own tick in
  `spawn_discovery`, after the browse, one at a time, only when a device has
  been unheard past `stale_after`.
- Match by `id=` only, and only against the unheard: a reply from a device that
  is happily streaming must never clear its resolution (that would drop its
  link). A different id at a known address is not a move.
- A match calls the existing `Registry::set_address`, which keeps the id, the
  player, the piece and the seed, and makes the next telemetry poll confirm the
  new address with `GET_INFO`.
