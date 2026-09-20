---
id: 111
title: Link should be constructible from a Device, not only a Target
type: build
hardware: no
depends: [011, 101]
owner: worker-111
branch: card/111-112-link-attach-sender-default
---

## Goal

Let an embedder hand `Link` a device it has already resolved, so a caller that knows
both ports does not have to go back through discovery - or through
`Device::from_addr`'s guess that the control port is frame + 1.

## Context

Feedback from card 101, which built the first real embedder. `Link::open` and
`Link::open_deferred` take a `Target`, and `Target::resolve` either browses or calls
`Device::from_addr(addr)`, which sets `control = frame + 1`. That is true of the
spec's defaults (49374/49375) and never true of an ephemeral pair, so:

- `crates/screeny`'s own tests reach a simulator through `Sender::connect(Device, ..)`,
  and `tests/simfix` has a comment explaining exactly this. `Link` has no equivalent
  door, so a `Link` test against a simulator has to make the simulator bind a
  **consecutive** port pair and hope both are free. `crates/art/tests/sender.rs` walks
  `50600..50680` looking for one. It works and it is silly.
- A service that resolves devices itself - which is what card 106's "players and
  devices" will be - has a `Device` in hand and has to throw it away.

Suggested shape, matching `Sender::connect`:

```rust
Link::attach(device: Device, cfg: LinkConfig) -> Result<Link>
Link::attach_deferred(device: Device, cfg: LinkConfig) -> Link
```

The reconnect path already re-resolves from `Link::target()`, so an attached link
either keeps the `Device` as its reconnect target (address only, no re-browse) or
carries an optional name alongside it. Decide which and say so in the doc comment:
"attached to an address, so it will not follow a DHCP lease" is a fine answer, but it
must be stated, because following a lease is the reason `--to <name>` is preferred.

A smaller alternative, if `attach` is not wanted: let `Target` carry an explicit
control port (`Target { addr, control: Option<SocketAddr>, .. }`), which fixes the
port-pair guess without adding a constructor.

## Deliverables

- The constructor (or the `Target` field), in `crates/screeny/src/embed.rs`.
- `crates/screeny/README.md`'s API block and the "Embedding" section updated.
- `crates/art/tests/sender.rs` simplified to use it, dropping the port walk.

## Acceptance

- A `Link` test can reach a `SimDevice::start(Config::for_test())` on ephemeral ports
  with no port arithmetic anywhere.
- `cargo test -p screeny` and `cargo test --release -p screeny-art --features sender`
  stay green.

## Log
