---
id: 080
title: A wire-level conformance suite the firmware can be run against too
type: test
hardware: no
depends: [006, 008]
owner: worker-080
branch: card/080-conformance-suite
---

## Goal

Card 006's receive-rule tests all drive `screeny-sim` in process. The firmware
(card 008) is the third implementation of the same spec and the one that
actually matters, and there is currently no way to ask it the same questions.
Lift the rules half of those tests into a suite that talks to *any* endpoint
over UDP, and run it against both.

## Context

- `crates/sim/tests/{sequence,arbitration,control,telemetry,malformed}.rs`
  already say what the rules are. They use `SimHandle::snapshot()` to check
  state the wire does not carry, which is precisely the part that does not port.
- Everything those tests assert about the wire is reachable over the wire:
  `TELEMETRY` carries all eight counters and the state byte, `BUSY` says who
  holds the lock, and `GET_INFO` says what the device claims to be. A
  conformance run can be written entirely in terms of those three.
- What cannot be checked over the wire is bit-exactness of the displayed frame.
  That needs the camera harness (card 012) on hardware, or `--dump-dir` against
  the simulator.
- `crates/sim/tests/common/mod.rs` is already a sender with an encoder per
  codec; it should move somewhere both can use it rather than being copied.

## Deliverables

- A crate or test target that takes `--addr host:frame_port` and a control
  port, runs the suite, and reports pass/fail per rule with the spec section
  cited.
- It must be safe to point at the real device: no `SET_WIFI`, no `REBOOT`, no
  flashing, and it must restore brightness and idle mode on exit.
- `crates/sim`'s own tests keep their in-process assertions (bit-exact pixels,
  events) and drop the duplicated wire-level ones.

## Acceptance

The suite passes against `screeny-sim` on loopback, and the orchestrator can
run it against the real device with one command and get a rule-by-rule report.

## Log
