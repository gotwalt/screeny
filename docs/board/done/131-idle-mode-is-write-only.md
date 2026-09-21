---
id: 131
title: The idle mode is write-only over the wire
type: design
hardware: no
depends: [080]
owner:
branch:
---

## Goal

Make the current idle mode readable, so that a tool which changes it can put
it back.

## Context

Found while writing card 080's conformance suite, which has to leave the
device exactly as it found it.

`SET_IDLE` (section 6.3) echoes the mode it just set. Nothing else reports
it:

- `TELEMETRY`'s 48 bytes (section 6.7) have a `state` byte but no idle mode,
  and the two are different things - section 7.5 is explicit that `HOLD_FOREVER`
  changes the state machine while `DIM` and `BLACK` change only what is drawn,
  so `state == IDLE` is consistent with modes 0, 2 and 3 alike;
- the TXT record (section 5.2) has no key for it;
- `GET_INFO` (section 6.6) is the same table.

The mode persists across reboot (section 6.3), so this is not a transient. A
tool that sets it has permanently changed the panel's behaviour with no way to
discover what it was before.

The suite works around this with `--restore-idle N`, defaulting to 0
(`STATUS`, the spec's documented default), and says so in its output. That is
a guess, not a restore.

Two ways out, either fine:

1. A `u8 idle_mode` appended to `TELEMETRY` under section 6.7's growth rule -
   one byte, no version bump. Pairs naturally with card 130.
2. An `idle=` TXT key, which also lets a sender see it without a round trip.

## Deliverables

- The decision in `docs/design/protocol-v1.md`, and the field or key in
  `crates/proto`, `crates/receiver`, `crates/sim` and `firmware/`.
- `screeny_probe::suite` reads the mode in its preflight and restores it,
  and `--restore-idle` becomes an override rather than the only mechanism.

## Acceptance

Run the conformance suite with the device in `HOLD_FOREVER` and it is still in
`HOLD_FOREVER` afterwards, without being told.

## Log

- 2026-09-21, firmware orchestrator: **closed as done by other means**, with the owner's agreement. The goal ("a tool that changes idle mode can put it back") is met over HTTP: `GET /api/v1/status` carries `idle_mode` (card 226, `crates/device-api/src/reply.rs`) and the probe's HTTP suite reads it and restores it for real instead of guessing. The UDP paths the card names (`TELEMETRY`, `GET_INFO`, TXT) still do not carry it and will not: every byte on the status structs costs core-0 stack (card 243b).
