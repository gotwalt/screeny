---
id: 016
title: Consolidate duplicated code (receiver core, frame types, panel model) and remove dead weight
type: build
hardware: yes
depends: [006, 008, 010]
owner: worker (card 016)
branch: card/016-consolidate-shared-code
---

## Goal

The build phase was done by parallel workers who were told not to touch each other's
crates, so the same logic now exists in several places. Before the art system merges
in and this becomes a long-lived codebase, put each thing in exactly one place.

## Context - the duplication, as found by the orchestrator

1. **Receive state machine, three copies' worth.** `crates/sim/src/core.rs` and
   `stats.rs` were hand-ported to `firmware/src/receiver.rs` and `rxstats.rs` (card
   008, with attribution; allocation strategy is the only intended difference). Any
   spec change now has to be made twice and will drift. Target: one `no_std`,
   no-alloc crate (`crates/receiver`, package `screeny-receiver`) that both `sim` and
   `firmware` use. The firmware's static-buffer/`heapless` shape is the constraint; the
   sim adapts to it, not the other way round. The sim's 97 tests and the probe's
   conformance suite are the safety net.
2. **Frame types.** `crates/demos/src/frame.rs` defines its own `Frame`/`Indexed`;
   `crates/screeny/src/frame.rs` has `Frame`; `crates/proto` owns `Rgb888Frame` /
   `IndexedFrame`. Make demos use proto's types (or screeny's thin wrapper) so
   `PieceSource` in `crates/screeny/src/main.rs` stops copying 6 KB per frame.
   NOTE: card 011 is changing `crates/screeny/src/sender.rs`, `frame.rs` and the
   README in parallel. Stay out of those files; if demos needs a type from screeny,
   depend on proto instead. Leave `main.rs`'s `PieceSource` adapter alone unless the
   change is two lines.
3. **Panel model / colour maths** exists in `crates/screeny` (`panel.rs`, `color.rs`),
   `crates/demos` (`panel.rs`, `color.rs`), `crates/sim` (panel LUT) and `lab/`.
   `lab/` is frozen - ignore it. For the three live crates: one implementation, in the
   lowest crate that makes sense (demos and sim may depend on `screeny`'s lib, or a
   small new `crates/panel`; your call, justify it). Card 071 (demos' byte estimator
   disagrees with the real encoder) dies with this: use the real encoder.
4. **Dead weight.** `spike/fw-skeleton/` is superseded by `firmware/` - delete it
   (history keeps it) and fix references. `firmware/` still has a
   `display-on-core0` A/B feature: keep (it is documented and cheap). Look for
   leftover throwaway code, unused deps (`cargo machete`-style by hand), stale
   comments that reference cards as future work that is done.
5. Tiny firmware fix while you are flashing anyway: `IDENTIFY` truncates the name one
   character early (`cut(name, 13)` where 14 fit) - card 008 left it deliberately.

## Bench rules (hardware: yes - only for verifying the firmware after the refactor)

- Read "Bench rules" in `docs/board/done/007-firmware-display.md` and
  `docs/board/done/008-firmware-network.md`. Tools by ABSOLUTE path in the main
  checkout (`/Users/aaron/src/screeny/tools/fw-run.sh <abs elf> NAME [secs]`), capture
  names prefixed `c016-`, baud <= 230400, one serial process at a time, never erase
  flash, never touch `backup/`. The device is 192.168.7.221.
- **Fast checks vs long evidence.** Iterate with host tests and the simulator. Flash
  only when the host side is green. On the device run `screeny-probe conformance`,
  `lock-test`, and ONE 60 s `stream --codec all`-style pass (or one 60 s run on the
  heaviest codec if `all` does not exist). No soaks. Never repeat a passing long run
  unless the firmware changed, and if you do, write why in the Log.
- Append results to the Log right after each run; commit after each step. Leave no
  background processes (`pgrep -lf 'screeny|espflash|qemu'`), and leave the device
  running the final firmware showing its status screen.

## Deliverables

- One receiver core used by `sim` and `firmware`; both copies in `firmware/src/`
  deleted; all sim tests pass; firmware builds; device passes probe conformance,
  lock-test and one 60 s stream with zero decode drops; flash/heap numbers before and
  after in the log (the refactor must not cost meaningful RAM).
- Frame types and panel/colour maths de-duplicated as described; card 071 closed.
- `spike/` removed, references fixed; unused dependencies removed.
- `cargo test --workspace` green; `cargo clippy --workspace` has no new warnings.
- `docs/design/architecture.md` updated to the new layout.

## Acceptance

Grepping the workspace finds one implementation each of: the receive state machine,
the panel transfer function, sRGB/Oklab conversion, the frame types. The device runs
firmware built from the shared core and behaves as it did in card 008.

## Log

### 2026-09-19 - step 1a: the shared receiver core, simulator side

New crate `crates/receiver` (`screeny-receiver`): `no_std`, no alloc, no float,
deps `screeny-proto` + `heapless`. It holds the receive state machine (sections
3.3, 4.7, 6, 7), the counters and EWMAs of 6.8, `Timing`, `State`, `DropCause`,
`ReleaseReason`, `Offer`, `Intent`, `FrameMeta` and the fixed-slot rate
limiters. Everything a receiver cannot decide for itself - the clock, where a
datagram goes, what happens to a decoded frame, whether there is a radio - is a
method on one `Host` trait. `Receiver<A>` is generic over the address type only
(`SocketAddr` here, `IpEndpoint` there), so the host can be a short-lived
borrow of an outbox.

The firmware's shape won, as the card says: the caller keeps the survivor
datagram and `offer_frame` answers `Keep`/`Drop`; `flush_frames` re-parses it
and decodes into a caller-owned `back`. The simulator adapts - `crates/sim`'s
`Core` is now a ~350-line adapter that owns the three frame buffers, the panel
model and the `Vec` outbox, and its public API is byte-for-byte what it was.

`crates/sim/src/stats.rs` is now a re-export; `config::Timing` is a re-export;
`event::{State, DropCause, ReleaseReason}` are re-exports and `Event` keeps its
owned `String` form with a `from_shared` conversion.

**No test was changed.** `cargo test --workspace`: 209 passed, 0 failed, same
as before the refactor. The sim's own count moves 97 -> 92 only because the
five EWMA unit tests moved with the code into `screeny-receiver` (97 = 92 + 5
either way).

Three deliberate, recorded behaviour differences, all of them the firmware's
shape winning:
* the simulator's three unbounded `HashMap` rate limiters become four-slot
  tables with oldest-entry eviction. Eviction can only make the device *more*
  generous to a source it forgot, which is the right direction to fail.
* `stats_req` holds two sources, not any number. Only the lock holder can have
  a frame accepted, so the second slot is for the drain in which a takeover
  happens; a third cannot arise.
* the firmware answered queued `STATS_REQ`s LIFO (`Vec::pop`), the simulator
  FIFO. The shared core is FIFO. It can only matter when two *different*
  sources ask in one drain, and only for the order of two datagrams to two
  different hosts.

One new event, `IdentifyExpired`, exists for the firmware's repaint; the
simulator drops it in `from_shared` so its event stream is unchanged.
