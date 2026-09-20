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

### Worker, step 4 (the schedule, and the test against two simulators)

**One new `Config` field** (`crates/studio/src/lib.rs`, the shared file - five
lines including its doc comment, plus one line in `Default`):
`probe_to: Vec<SocketAddr>`. Empty, the default, means the subnet broadcast
address of every interface and the probe then follows `discover`, so
`--no-discover` turns it off exactly as the card asks. A non-empty list is
"ask these control addresses instead", which is what a container with no
broadcast route needs and what the tests use. **No new CLI flag**; the two
help lines that said "browse" now say "browse or probe".

**The schedule** (`crates/studio/src/fleet.rs`): `spawn_discovery` now runs a
probe after the browse on the same tick.

- *only when something is missing*: `Registry::unheard(stale_after)` is empty,
  so nothing is sent. A house that is working sends nothing at all.
- *one in flight*: it is awaited on the one discovery task, after the browse -
  so a probe can never delay a browse, and nothing here ever touches a player
  (the supervisor re-aims the link on its next 1 s tick).
- *capped, jittered backoff*: the existing `fail()`, under one key - the probe
  is one thing for the fleet, not one per device. Twelve passes at the cap.
- *window*: 1 s, clamped to the tick.
- *logged per event*: one line when a panel is followed, one line when the
  probe starts failing for a *new* reason, and silence for ever when it finds
  nothing. `/api/v1/status` carries `discovery.probes`, `discovery.moved` and
  `discovery.last_probe_error` for the rest.
- *never on a render thread*: `spawn_blocking`.

**The test** (`crates/studio/tests/moved.rs`, new file, two tests). mDNS off,
loopback only, the probe pointed at a small band of loopback control ports -
the file's stand-in for a broadcast, since the studio is never told which of
them the panel moved to and learns it from the `id=` in the answer. Each test
has a band of its own, because they run at the same time.

- `a_panel_that_moved_is_followed_and_the_other_is_left_alone`: two
  simulators, each playing something chosen; one is dropped and comes back on
  a different port with the same id. The studio follows it with nobody saying
  anything - same id, same player, same piece (`metaballs`), same seed (4242),
  same fps, still two devices, still the same panel on the page - while the
  other keeps its address, its resolution, its link (`sessions` unchanged) and
  its frames. Measured: `mov001` moved 50940 -> 50944 and was followed; the
  stayer went 7 -> 43 frames without a reconnect.
- `the_status_poll_follows_a_panel_that_moved`: card 180's poll resumes by
  itself at the new resolution (`http.reads` 1 -> 2, `facts` 0.0 s old, no
  error). That `http_addr` is the *new IP* is the unit test, because loopback
  has only one IP.

**Two things found while writing it, both worth writing down.**

1. `Registry::heard_http` counts as hearing from a device - correctly - so on
   loopback, where the replacement simulator takes the **same** HTTP port
   back, a panel that is replaced instantly never goes unheard and is never
   probed for. The test therefore leaves the panel away for longer than
   `stale_after`, which is what a panel taking a new DHCP lease does anyway.
   On a real move the IP changes and UDP and HTTP stop together, so this is a
   property of loopback rather than of the product.
2. `POST /api/v1/devices/add {play:true}` **attaches** the panel it adds, so
   the last panel added is the one the page shows. Not this card's business;
   the test asserts that following a panel does not change which one is
   attached, rather than which one that is.

`cargo test -p screeny-studio --test moved`: 2 passed, 0 failed, in 6.9 s.

### Worker, step 5 (the test was flaky; it is not now)

Running the new test twelve times in a row caught it failing twice, and the
failure was worth the trouble: the panel that **stayed** was `"resolved":
false` at the moment the assertion read it, while its link was up and it was
still streaming (`connected: true`, `sessions: 1`, `last_seen_ago: 2.0`).

Why: `poll_once` walks the devices in turn, and a device at a dead address
costs the pass its control timeout (`CONTROL_TIMEOUT`, 400 ms). With the
test's `stale_after` at **one** second, the healthy panel's turn could come
late enough that it was marked stale for a pass - and then re-resolved on the
next one. Nothing to do with the probe, and not a product problem either: the
product's `stale_after` is 120 s, so the same thing needs two minutes of
silence from a panel whose link is up, which is a panel whose telemetry really
has stopped. It is a property of compressing that line to a second in a test.

Fixed in the test, twice over: `stale_after` is 3 s with a comment saying why
it is not shorter, and the "panel that stayed" conditions are part of the
`until_json` predicate, so everything asserted comes out of **one** read
rather than a read taken after a wait for something else.

`cargo test --release -p screeny-studio --test moved`, fourteen consecutive
runs after the fix: 14 x `ok. 2 passed; 0 failed`, 6.4-10.3 s each. (Before
the fix: 10 ok, 2 failed out of 12.)

### Worker, step 6 (docs)

`crates/studio/README.md`: a bullet under "one panel, one picture" saying what
the probe is, when it happens, what it will not do (a panel reached by name, an
id this studio does not know) and that `--no-discover` turns it off; the
`--no-discover` row and the "nothing grows without bound" sentence now mention
the probe. `crates/screeny/README.md` got its two API lines in step 2.
`docs/design/protocol-v1.md` is **not** touched: 5.5 already says exactly this
and the wire is unchanged - one `GET_INFO`, the same reply, no new opcode.

### Worker, step 7 (verification, and what the orchestrator should run)

**Everything as actually run, on this branch, with no hardware and no LAN
access:**

- `cargo test --release --no-fail-fast` at the root: **724 passed, 0 failed, 1
  ignored** across 80 test binaries and doc-test targets, exit 0.
- `cargo clippy --workspace --all-targets`: **silent**, no warnings, and no
  `#[allow]` added anywhere in this card.
- `cargo test --release -p screeny-studio --test moved`, **14 consecutive runs
  after the de-flake: 14 x ok, 2 passed, 0 failed** (6.4-10.3 s each).
- Nothing left running: `pgrep` for cargo/rustc/screeny is empty, and `lsof`
  shows nothing listening on 49374/49375 or on the 50900-51100 test band.

**One flake seen that is not mine**, written down so the next person does not
chase it: `screeny-sim`'s `a_sender_can_tell_network_loss_from_a_slow_device`
(`crates/sim/tests/telemetry.rs`) failed once under a full parallel `--release`
suite with `frames_dropped_superseded 1, expected 0`, and passed 10/10 on its
own and in three later full runs. It paces a sender at 4 ms on loopback and
asserts the simulator superseded nothing, which a loaded machine can break.
Nothing in this card touches the frame path, the simulator or `proto`.

**Two things I did not do, and why.** The probe still waits its whole 1 s
window even once every missing panel has answered: it is one second on a
blocking thread, so stopping early would buy nothing worth the machinery. And
`Config::probe_to` has no CLI flag - that is new card **142**, because a
container whose broadcast does not reach the panel needs one and this card did
not ask for it.

### The real-panel check, for the orchestrator

No hardware was touched here. This is the bench half of the acceptance.

1. Build from this branch and run the studio the normal way - discovery on, so
   the probe is on: `cargo run --release -p screeny-studio`.
2. Add the panel **by address**, not by name: a name already follows a lease,
   so testing with one would not be testing this card. `POST
   /api/v1/devices/add` with `{"to":"192.168.7.221","name":"desk","play":true}`.
   Wait until `/api/v1/status` shows it `"resolved": true` and streaming, then
   set something recognisable with `POST /api/v1/player/set`:
   `{"device":"4a00a4","piece":"metaballs","seed":4242}`.
3. Make it move: give it a different DHCP lease (a reservation change and a
   power cycle is the easy way on this bench), and **leave the studio alone**.
4. What to expect, within `stale_after` (120 s) plus a browse tick (30 s) and
   possibly a few backoff passes - so a couple of minutes, not seconds:
   - exactly **one** new line on stderr, of the form
     "studio: `4a00a4` answered a probe at 192.168.7.NN:49374 (it was at
     192.168.7.221:49374); following it";
   - in `/api/v1/status`: `devices[0].address` is the new address,
     `"resolved": true`, `control_addr` is the new IP on 49375, and the player
     is still on `metaballs` seed 4242 with `panel.connected: true` and
     `frames_sent` climbing. `discovery.moved` is 1 and
     `discovery.last_probe_error` is null;
   - `/healthz` 200 throughout, and the panel showing the same piece it was
     showing. The point of the card is that it does not restart.
5. **Which mechanism caught it.** On a bench where mDNS works, the browse may
   win the race and re-resolve the panel first. That is fine and is not a
   failure of this card: `discovery.moved` and the log line above are what say
   the probe did it, and if the browse got there first, `moved` stays 0 and
   there is no line. To watch the probe do the work alone, the panel has to be
   known by address (step 2) and out of mDNS's reach - which is the container,
   where it already is.
6. **If it does not catch up**, the two honest failures are: `discovery.probes`
   climbing while `moved` stays 0 (the panel is not answering the broadcast, or
   the broadcast does not reach it - `discovery.last_probe_error` says if the
   socket itself refused), and `probes` staying 0 (nothing counted as unheard -
   compare `devices[0].last_seen_ago` with 120 s). Nothing needs restarting to
   retry; the next tick tries again.

### Orchestrator, after the merge (2026-09-20)

Reviewed `fleet.rs` (the probe rides the browse's tick, after it, only while something is
unheard) and `discover.rs` (frame port = `ctrl - 1`, the same convention a bare `Target`
already uses). Merged `--no-ff`; root `cargo test --release --no-fail-fast`: 741 passed, 0
failed; clippy silent. The acceptance line - move the bench panel to a new DHCP lease - needs
a change at the router and a power cycle, which is the owner's to do when he cares to; step 7
above says what to look for. Until then the evidence is the two-simulator test. Note the
deployed Studio reaches the panel by *name*, which already follows a lease: this matters for
panels added by address and for the container, where mDNS is the thing most likely missing.
