---
id: 016
title: Consolidate duplicated code (receiver core, frame types, panel model) and remove dead weight
type: build
hardware: yes
depends: [006, 008, 010]
owner:
branch:
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
