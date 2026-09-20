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

### Worker, step 2 (`screeny::discover`: a probe you can point somewhere)

`crates/screeny/src/discover.rs`, and nothing else in that crate:

- **`pub fn broadcast_targets(port) -> Vec<SocketAddr>`** - the subnet
  broadcast address of every usable IPv4 interface, lifted out of
  `broadcast_probe` unchanged (same filter, same sort/dedup, same
  `255.255.255.255` fallback).
- **`pub fn probe(timeout, &[SocketAddr])`** - the old body, with the
  destinations passed in. One socket, one datagram per destination, one
  window.
- **`broadcast_probe(timeout, port)`** is now exactly `probe(timeout,
  &broadcast_targets(port))`, so `screeny --broadcast` is bit-for-bit what it
  was.
- One behaviour change, deliberate and stated: the frame port in a probe
  result was `DEFAULT_FRAME_PORT`; it is now `info.ctrl - 1`, the same `+1`
  convention `Device::from_addr` and `--addr` have always used. **For a device
  on the spec's ports these are the same number** (49375 - 1 = 49374), so no
  real panel sees a difference; it is what lets a probe reach a simulator on a
  port pair of its own. A reply with `ctrl=0` is ignored rather than
  underflowing.

Tests (hermetic, no multicast, no LAN): `a_probe_can_be_pointed_at_named_addresses`
stands up a hand-rolled loopback responder (no dev-dependency on `screeny-sim`,
which depends on this crate), probes two addresses of which one is dead, and
checks the answer names the device by its `id=` and that frame = ctrl - 1;
`the_default_probe_targets_are_the_subnet_broadcasts` pins that the default
destinations are IPv4, on the port asked for, and never loopback.

`cargo test -p screeny --lib discover`: 10 passed, 0 failed. Card 176's
`a_named_browse_returns_when_that_instance_answers` still passes untouched -
nothing here goes near `collect_until`, `browse` or `browse_for_name`.
`crates/screeny/README.md`: two lines in the API summary.

### Worker, step 3 (the registry half)

`crates/studio/src/devices.rs`:

- `Registry::unheard(stale_after) -> Vec<String>` - who a probe is *for*:
  devices with a real id that have not been heard from in longer than
  `stale_after` (never heard counts, which is a studio that has just
  restarted). A `pending:` id is excluded: nothing has ever spoken to it, so no
  `id=` in a reply can be matched against it.
- `Registry::probed(found, candidates, error) -> Vec<Moved>` - the matching
  rule, by `id=` and nothing else. Same id at a new address: `set_address`,
  which keeps the id, the player, the piece and the seed and makes the next
  telemetry poll confirm the address. An unknown id: not this studio's
  business. A *different* id at an address we hold: **not** a move, and the
  record holding that address is untouched.
- Two more exclusions I decided while writing it, both worth stating:
  - **a device reached by *name* is left alone.** A name re-resolves on every
    reconnect and already follows a lease (the card's own opening paragraph);
    overwriting `stored.address` with a literal address would take that away
    permanently, because `reach()` prefers the address over the instance name.
  - **a device that answers from the address we already hold is not a move.**
    It is alive and the telemetry is what is failing; re-setting the address
    would throw away a good resolution for nothing.
- `devices::probe(timeout, &[SocketAddr])`, the blocking wire call, beside
  `browse`.
- `DiscoveryHealth` gains `probes`, `moved`, `last_probe_error`. Additive, so
  `/api/v1/status` carries them with no change to `health.rs` or `api.rs`.

Three unit tests, all pure: `a_probe_is_only_for_devices_that_have_gone_quiet`,
`a_probe_follows_a_device_that_answers_from_a_new_address` (which also pins
that `http_addr` is the **new** IP once the poll has confirmed it - the card's
"verify it rather than assuming it", in the form that cannot be shown on
loopback because loopback has only one IP), and
`a_probe_leaves_alone_everything_it_was_not_about` (unknown id, a different id
at a known address, a name-reached device, a device that is streaming, and the
same-address case).

`cargo test -p screeny-studio --lib devices`: 14 passed, 0 failed.
