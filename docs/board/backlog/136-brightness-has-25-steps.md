---
id: 136
title: The brightness control has 25 real steps and nothing says so
type: design
hardware: no
depends: [020, 066]
---

## Goal

Write down what `SET_BRIGHTNESS` actually does, so a sender stops believing it
has 256 settings and a user stops wondering why brightness 3 is black.

## Context - found while doing card 066

The device dims by shortening the output-enable window:
`firmware::display::slots_for(b) = (b * 25 + 127) / 255`, lit slots out of
`MAX_OE_SLOTS = 25`. Two consequences the spec does not mention:

- **The resolution is 25 steps, not 256.** Brightness 129 and 130 light the
  same 13 slots and are the same picture. A UI slider with 256 positions is
  lying about 231 of them.
- **Brightness 1..=5 is fully off.** `slots_for(5) = 0`. The device accepts the
  value, echoes it back as `applied`, reports it in telemetry, and shows
  nothing. `applied` is documented as "the value actually in effect, which is
  how a sender learns the cap" (spec section 6.6), and here it is not.

Neither is a bug in the firmware - 25 slots is the power budget - but
`docs/design/protocol-v1.md` section 6.6 describes `SET_BRIGHTNESS` as
`u8 level (0-255, clamped to the firmware cap)` and stops there.

`screeny_panel::{MAX_OE_SLOTS, oe_slots, oe_light}` (card 066) are the host-side
copy of the arithmetic; anything that wants to show steps can use them.

## Deliverables

- A paragraph in `docs/design/protocol-v1.md` section 6.6 on the real
  resolution and on the off-by-duty floor. Decide, with the owner, whether
  `applied` should snap to the lowest brightness that lights a slot (a
  behaviour change in `crates/receiver`, shared by firmware and sim) or whether
  the spec simply says `applied` is the requested value after clamping and a
  sender is expected to know about duty.
- Whatever the decision, the simulator and the firmware agree, and a test in
  `crates/receiver` pins it.
- If a brightness control ever appears in the studio, it steps in slots.

## Acceptance

The spec answers "what does brightness 3 do" and the two receivers do the same
thing.
