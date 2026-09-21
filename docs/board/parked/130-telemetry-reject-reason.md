---
id: 130
title: Telemetry cannot say *why* a datagram was rejected
type: design
hardware: no
depends: [080]
owner:
branch:
---

## Goal

Decide whether `TELEMETRY` should carry the reason for the most recent
rejection, and if so append it per spec section 6.7's growth rule.

## Context

Found while writing card 080's conformance suite.

`frames_rejected` (section 6.7) is one counter for six different faults: bad
magic (2.1), a version that is not 1 (2.2), a reserved packet type (2.2), a
`CONTROL` on the frame port (2.2), `8 + len` past the datagram or `len` over
1464 (2.3), and a frame from a source that does not hold the lock (7.4).

`crates/sim` distinguishes them internally - `screeny_proto::Reject` has a
variant per case and `crates/sim/tests/malformed.rs` asserts the exact reason
for each - but **an outside observer cannot**. So every negative framing rule
in `screeny_probe::suite::framing` can only assert "the counter went up by
one and nothing came back". Against the simulator that is backed up by
`malformed.rs`; against the firmware it is all there is, and a firmware that
rejected a good frame for the wrong reason would still pass.

Section 6.7 already says how to grow: "a future device MAY append fields and
set a larger `len`; a v1 sender MUST read only the first 48 bytes and ignore
the rest". So a `u8 last_reject` at offset 48 costs one byte and no version
bump.

Against it: telemetry is for diagnosing a *stream*, and section 6.9's table is
deliberately small. A reject reason is a debugging aid, and the debugging aid
that already exists is the serial console.

## Deliverables

- A decision, recorded in `docs/design/protocol-v1.md` section 11, either way.
- If yes: the field in section 6.7, `screeny_proto::control::Telemetry`,
  `crates/receiver`, `crates/sim`, and a rule per reason in
  `screeny_probe::suite::framing` replacing today's "counted, and silent".

## Acceptance

Either the spec says why not, or a conformance run can tell a rejected magic
byte from a rejected version.

## Log

- 2026-09-21, firmware orchestrator: **parked** with the owner's agreement, under decision 10 of `docs/design/device-web.md` ("not a commercial product, we don't need to overly bomb-proof it"). A read-only survey against fw 0.7.0 found it still open as written and not a crash or memory risk. Not picked up without the owner asking.
