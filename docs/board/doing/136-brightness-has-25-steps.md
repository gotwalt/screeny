---
id: 136
title: The brightness control has 25 real steps and nothing says so
type: design
hardware: no
depends: [020, 066]
owner: sonnet worker (firmware session, 2026-09-21)
branch: card/136-brightness-steps
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

## Decision (owner, 2026-09-21)

**Snap up to the dimmest lit level.** A nonzero `SET_BRIGHTNESS` request below the
off-by-duty floor (1..=5 today) becomes the lowest level that lights one slot, and
`applied` reports that level. 0 stays off. Above the floor nothing changes: `applied` is
the request after the cap, as now (no snapping to slot representatives). The floor is
derived from the slot arithmetic (`MAX_OE_SLOTS`, the rounding in `slots_for` /
`screeny_panel::oe_slots`), not written as a literal 6. The same rule applies wherever a
stored brightness is loaded (settings store, HTTP settings) if those paths go through
`crates/receiver`; if they do not, say so in the Log and make them agree.

This is the last firmware card of this cycle (owner, 2026-09-21). Do **not** bump
`FW_VERSION`: cards 246 and 230 are in flight and the orchestrator sets the version at
the merge.

## Log

- Branched `card/136-brightness-steps` from `main` at 8081857.
- Step 1: `crates/receiver/src/lib.rs` gets a "Brightness (card 136)" section
  (after `Timing`, before `Vocabulary`): a private `oe_slots` (the same
  arithmetic as `firmware::display::slots_for` / `screeny_panel::oe_slots`),
  a derived `pub const BRIGHTNESS_FLOOR: u8` (a `const fn`-computed loop over
  `oe_slots`, not a literal - today it evaluates to 6), and
  `pub const fn clamp_brightness(level: u8, cap: u8) -> u8`: clamps to `cap`,
  then snaps a nonzero, sub-floor result up to `BRIGHTNESS_FLOOR`; 0 stays 0.
  **Decision recorded here:** if `cap` itself is below the floor, the cap
  wins - `clamp_brightness(200, 3) == 3`, still possibly black, because
  `SET_BRIGHTNESS` must never report `applied` above the cap it was just
  given (spec 6.3).
  `Receiver::apply`'s `Request::SetBrightness` arm (was
  `level.min(self.brightness_cap)`, ~line 1269) and `Receiver::new` (was
  `p.brightness.min(p.brightness_cap)`, ~line 631) both now call
  `clamp_brightness` - the one place the rule lives.
  `crates/receiver` stayed `no_std`/heapless-only; `screeny_panel` (std,
  `f32`) and `firmware` (a separate cargo project) are unreachable from it, so
  the arithmetic is a third copy on purpose, tied down by
  `crates/receiver`'s own `brightness_tests` module and by a new test in
  `crates/panel/src/model.rs` (`receivers_brightness_floor_matches_this_crates_oe_slots`,
  with `screeny-receiver` added as a `[dev-dependencies]`-only edge in
  `crates/panel/Cargo.toml` - never built for the firmware) that pins
  `screeny_receiver::BRIGHTNESS_FLOOR` against `screeny_panel::oe_slots`.
  `cargo test -p screeny-receiver` and `-p screeny-panel` both green;
  `cargo build --workspace` clean.
