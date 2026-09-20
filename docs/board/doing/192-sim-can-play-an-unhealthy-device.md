---
id: 192
title: The simulator can only play a healthy device
type: build
hardware: no
depends: [224, 180]
owner: worker-192 (firmware session)
branch: card/192-sim-unhealthy-device
---

## Goal

`screeny-sim` should be able to report a panel that is **not** well: a brownout or a
panic as the reset reason, a firmware slot that is `pending_verify` or `invalid`,
settings-store errors, a nearly-full heap, a nearly-exhausted stack.

**This is `crates/sim`, which the firmware session owns.** Written here by card 180's
worker because that is where the gap was found; it is theirs to pick up or refuse.

## Context

- `crates/sim/src/api.rs::status` reports `FwSlot::Ota0`, `FwState::Valid` and
  `ResetReason::PowerOn` as constants, and `heap_used`/`heap_size`/`stack_free`/
  `store_errors` from `core::Ident`'s fixed values (64 KB of 96 KB, 20 KB, 0). The
  comment is honest about why: there is no flash and no reset to have a reason.
- Card 180 gave the Studio's page four things that are meant to stand out - a reset
  reason that is not a power-on or a reboot we asked for, `store_errors > 0`, a
  `fw_state` that is not `valid`, and memory running out - and **none of them can be
  produced by the simulator**. They are covered by unit tests over
  `crates/device-api/tests/golden/status.json` and by a hand-written test server in
  `crates/studio/tests/device_status.rs`, but the whole page has never been seen in its
  unhappy state against a device-shaped thing.
- That matters more than it sounds. These four are exactly the rows nobody will ever see
  in the ordinary course of things, so they are exactly the rows that will be wrong when
  they finally appear. The owner's panel reading `brownout` at three in the morning is
  the moment they have to be right.

## Deliverables

Flags on `screeny-sim` that set what `GET /api/v1/status` reports, with nothing else in
the simulator pretending they are real:

- `--reset-reason NAME` (any `screeny_device_api::ResetReason`)
- `--fw-slot`, `--fw-state`
- `--store-errors N`
- `--heap-used N --heap-size N`, `--stack-free N`

## Acceptance

`screeny-sim --headless --no-mdns --http-port N --reset-reason brownout --store-errors 3
--stack-free 900` answers a status with those values, and the Studio's page draws all
three in the fault tone. A screenshot of that page goes in the card's Log - it is the
only picture of the unhappy state anyone has.

## Log
