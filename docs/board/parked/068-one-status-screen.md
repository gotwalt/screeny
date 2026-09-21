---
id: 068
title: The simulator and the firmware each draw their own status screen
type: build
hardware: yes
depends: [016]
---

## Goal

One idle/status/IDENTIFY screen, drawn by one piece of code, so that what the
simulator shows is what the panel shows.

## Context - found while doing card 016

Card 016 consolidated the receive state machine, so `crates/sim` and
`firmware/` now agree about *when* the idle screen is up (`Intent`), and about
the name, the address and the state it should show. They still disagree about
what it looks like, because they draw it twice:

* `firmware/src/screens.rs` (about 200 lines) uses `embedded-graphics` with
  its built-in `FONT_5X7` and `FONT_4X6`.
* `crates/sim/src/screens.rs` (393 lines) and `crates/sim/src/font.rs` (254)
  are a second implementation with hand-made column-major 5x7 and 3x5 fonts.

Two screens means the RSSI bars, the name truncation (`cut(name, 14)` on the
device after card 016) and the cross-fade can all drift, and the simulator is
supposed to be the thing you check a change against when you do not have the
bench.

Not in card 016's scope: the card named the receive state machine, the frame
types, the panel model and the dead weight, and the screens are none of those.

## Notes for whoever takes it

- `embedded-graphics` builds on the host, so the direction of travel is
  probably "the simulator uses the firmware's drawing code", not the reverse -
  a small `no_std` `screens` crate over `embedded-graphics`, with the pixel
  sink abstracted the way `screeny-receiver`'s `Host` is.
- `crates/demos/src/font.rs` is **not** part of this. It is a proportional
  hand-drawn font that exists because "TWENTY FIVE" does not fit on a 5 px
  monospace grid; it is art, not chrome.
- Hardware: one camera still of the status screen and one of IDENTIFY, before
  and after, is the evidence. Nothing else needs the bench.

## Acceptance

`grep -rn "fn status" crates firmware` finds one definition, and a camera still
of the device's idle screen matches a simulator screenshot of the same state.

## Log

- 2026-09-21, firmware orchestrator: **parked** with the owner's agreement, under decision 10 of `docs/design/device-web.md` ("not a commercial product, we don't need to overly bomb-proof it"). A read-only survey against fw 0.7.0 found it still open as written and not a crash or memory risk. Not picked up without the owner asking.
