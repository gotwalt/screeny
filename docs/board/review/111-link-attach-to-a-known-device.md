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

### 2026-09-19 - the shape: `attach`, not a control port on `Target`

Took the card's suggested shape (`Link::attach` / `Link::attach_deferred`) over the
`Target { control: Option<SocketAddr> }` alternative:

- `Target` is *how to find a device* - it is parsed straight from the CLI's
  `--addr`/`--name`/`--timeout`/`--broadcast` flags. A control port is not a way of
  finding anything; putting one there makes `Target` a half-resolved `Device` and
  leaves `resolve()` with a new combination to explain (a name *and* a control port -
  which wins?).
- A resolved `Device` carries more than the two addresses: `instance`, `host`,
  every address it resolved to, and the `DeviceInfo` from the TXT record. A caller
  that browsed once - card 106's "players and devices" - has all of it and would
  have to throw it away to squeeze back into a `Target`.
- It matches `Sender::connect(Device, SenderConfig)` exactly, so `Link` is now the
  same two doors as `Sender` with reconnection above them. Nothing existing changed
  shape; `open`/`open_deferred` keep their signatures and their behaviour.

**Reconnect behaviour, stated as the card demands:** an attached link reuses the
`Device` verbatim on every retry. It never browses, so it comes back after a reboot
at the same address and does **not** follow a DHCP lease. The doc comment says so and
says what to use instead (`Link::open` with `Target { name }`). Deliberately *not*
re-browsing on the device's `instance`: for a device built by `Device::from_addr`
that field is a synthesised `"127.0.0.1:53551"`, so browsing on it would silently
turn an address the caller chose into a name it never asked for.

Implementation: one private `enum Aim { Find(Target), Known(Device) }` is what a
connection attempt carries (it is what gets moved onto the connect thread), and
`Link` holds `attached: Option<Device>` beside the target it already had. Both
connect paths - `connect_now` for the blocking constructors and `spawn_attempt` for
the background retry - go through it, so there is one resolution rule and not two.

Three small additions beyond the two constructors, each because leaving it out would
have been a hole rather than restraint:

- `Link::attached() -> Option<&Device>`: reads the pinned device back, control port
  and all, which `target()` cannot express.
- `Link::reattach(Device)`: `retarget`'s counterpart. `retarget` had to learn to
  clear the pin anyway (otherwise it would silently do nothing to an attached link),
  and once that existed, the symmetric move-the-pin case is three lines.
- `target()` for an attached link answers `Target { addr: Some(device.frame) }`, so
  it is still honest rather than empty.

`cargo clippy -p screeny --all-targets` is clean; `large_enum_variant` on `Aim` is
allowed with a reason (boxing would buy an allocation on a value that lives for one
connect attempt).

### 2026-09-19 - tests, with no port arithmetic anywhere

`crates/screeny/tests/embed.rs` gained three tests, all against
`SimDevice::start(sim_config(0, 0))` - the fixture's ephemeral pair, mDNS off:

- `an_attached_link_reaches_a_device_on_ephemeral_ports` - asserts up front that the
  control port is *not* frame + 1, so the test would be vacuous if it ever became a
  consecutive pair, then pushes a frame and checks the simulator drew it
  palette-exact.
- `an_attached_link_reconnects_to_the_same_two_ports` - the sim is dropped and
  restarted on the pair it had; the link notices, reconnects and lands on the same
  control port, with the frame accounting invariant intact.
- `reattach_moves_a_pinned_link_to_another_known_device` - two simulators, the pin
  moved between them mid-stream, lifetime totals carried over, then `retarget` puts
  it back on the discovery path and `attached()` goes to `None`.

`cargo test -p screeny --test embed`: 17 passed, 0 failed, 1.02 s. No port is chosen,
guessed or incremented anywhere in the three new tests.

`crates/screeny/README.md`: "Embedding" gained the paragraph (what `attach` is for,
and the "pinned sockets, no browse, no DHCP lease" sentence), and both API blocks and
the `tests/embed.rs` row of the testing table list the new calls.

### 2026-09-19 - the art test drops its port walk

`crates/art/tests/sender.rs` used to loop `50600..50680` looking for a free
*consecutive* pair, because `SenderOutput` could only be given a `Target`. It now
starts one simulator on `Config::for_test()` (both ports ephemeral), builds the
`Device` that describes it, and hands that to a new
`SenderOutput::attach(Device, LinkConfig)` - three lines of fixture instead of a
retry loop, and no port chosen, guessed or incremented in the file at all. The
`start_sim` doc comment that used to explain the frame + 1 guess now explains why
there is nothing to explain.

`SenderOutput::attach` is the one addition to `crates/art/src/output/sender.rs`: the
same one-line wrapper `open_with` is, over `Link::attach` instead of `Link::open`,
with the pinned-reconnect caveat repeated in its doc comment. `target_for` and
`SenderOutput::open_with` are untouched and still used by `screeny-art play` and by
the studio (card 105), so nothing existing moved.

`cargo test --release -p screeny-art --features sender --test sender`: 3 passed,
0 failed, 2.06 s.

### 2026-09-19 - acceptance and what was left alone

- A `Link` test reaches `SimDevice::start(Config::for_test())` on ephemeral ports
  with no port arithmetic anywhere: `an_attached_link_reaches_a_device_on_ephemeral_ports`
  (and two more beside it).
- `crates/art/tests/sender.rs` has no `50600..50680` walk and no port arithmetic at all.
- `cargo test -p screeny` and `cargo test --release -p screeny-art --features sender`:
  green, as part of a green root `cargo test --release --no-fail-fast`.
- `cargo check -p screeny-studio` (card 105's crate, untouched): clean, 53.8 s - the
  additive `SenderOutput::attach` did not disturb the existing constructors.
- No `crates/sim` change was needed: the fixture only had to ask for port 0 twice,
  which `Config::for_test()` already does.
- Not done, deliberately: nothing was migrated to `attach` beyond the test.
  `screeny-art play --to` and the studio both take a name or an address from a human
  and should keep resolving it, because that is what follows a DHCP lease. The
  natural next caller is card 106's device list, which will hold `Device`s.
