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
- Step 2: the other paths that reach the display.
  - **Settings store load at boot** (`firmware/src/main.rs:808`, was
    `settings.brightness.min(BRIGHTNESS_CAP)`): now
    `screeny_receiver::clamp_brightness(settings.brightness, BRIGHTNESS_CAP)`.
    This was the one path that bypassed `crates/receiver` entirely - it feeds
    `BRIGHTNESS` (the atomic `display::slots_for` reads to size the
    framebuffers' OE window) and ran before `Core::new` even existed. Before
    this fix a stored 3 would come up with the panel dark and, because
    `Core::new` separately seeded the receiver's own `brightness` field from
    the same raw, unclamped `settings.brightness`, `TELEMETRY`/status would
    have agreed it was 3 - consistent with each other, both wrong. Now both
    read 6 (today's floor) and the panel is lit.
  - **`Core::new`** (`firmware/src/receiver.rs:271-275`, was
    `settings.brightness.min(crate::BRIGHTNESS_CAP)` passed into `Params`):
    simplified to pass the raw `settings.brightness` - `Receiver::new` already
    applies `clamp_brightness` with `p.brightness_cap`, so the pre-clamp was
    redundant once step 1 landed and is now just the one call site.
  - **`SET_BRIGHTNESS` over UDP control** and **`POST /api/v1/settings`**
    (`firmware/src/http.rs`'s `post_settings` -> `apply_control` ->
    `core.control(...)`): both already funnel through
    `Receiver::apply`'s `Request::SetBrightness` arm - no separate clamp in
    `http.rs` or `crates/device-api` (`SettingsRequest.brightness` is a bare
    `Option<u8>`, no validation of its own). Unchanged, already correct once
    step 1 landed.
  - **Telemetry / status** (`Receiver::telemetry`, `GET /api/v1/status`,
    `POST /api/v1/settings`'s reply): all read either `crate::BRIGHTNESS` (the
    atomic, now only ever written with a clamped value) or
    `core.brightness()` / `self.brightness` inside `crates/receiver` (set only
    through `clamp_brightness`, in `Receiver::new` and in `apply`). Nothing
    to change; they now report the same value the panel shows.
  - **`crates/sim`** (`crates/sim/src/core.rs::Core::new_with`): already
    passed `cfg.brightness` straight into `Receiver::new` with no separate
    clamp, and read `r.brightness()` back for the panel model's own
    brightness - it inherited the fix for free.
  - Firmware build: `firmware/` builds clean under the Xtensa toolchain
    (`. ~/export-esp.sh`, `cargo build --release`). `tools/fw-size.sh` on
    `target/xtensa-esp32-none-elf/release/screeny-fw`: `.stack = 26200`
    (floor 24576) - unchanged in shape from before this card (no new
    statics, only a different call in an existing const-fn-sized path).
    `FW_VERSION` left untouched, as directed.
- Step 3: tests.
  - New tests live in `crates/receiver/src/lib.rs`'s `brightness_tests`
    module (pinned in step 1's commit): `zero_stays_off`,
    `one_to_five_snap_to_the_floor`, `the_floor_itself_is_unchanged`,
    `above_the_floor_nothing_changes` (129, 130), `the_cap_still_applies_above_the_floor`,
    `a_cap_at_or_above_the_floor_still_snaps_up`, `a_cap_below_the_floor_wins`,
    plus `floor_is_six` pinning `BRIGHTNESS_FLOOR` and the underlying
    `oe_slots(5)==0` / `oe_slots(6)==1` it is derived from.
  - `crates/panel/src/model.rs`:
    `receivers_brightness_floor_matches_this_crates_oe_slots` (new,
    `crates/panel/Cargo.toml` gained `screeny-receiver` as a
    `[dev-dependencies]`-only edge for this one test).
  - Checked `crates/sim/tests/{control,telemetry,health,http_routes,http_conformance,core_rules}.rs`
    for `SET_BRIGHTNESS`/settings values below today's floor (6): none found -
    every fixture uses 0, 30, 40, 77, 88, 96, 100, 120, 255, or a cap of 100/120,
    all either 0 or comfortably above the floor. No sim test needed updating.
  - `crates/probe/src/suite/control.rs`'s `brightness_applies` conformance
    rule *did* assume an exact echo of `found / 2`, which breaks once `found`
    is small enough that half of it lands below the floor (this rule runs
    against a real device or the simulator via `screeny-probe`, not under
    `cargo test`, so it would not have failed a CI run - it would have failed
    on the bench). Fixed to predict the expected reply with
    `screeny_receiver::clamp_brightness(low, 255)` instead of asserting
    `applied == low` (`crates/probe/Cargo.toml` gained a `screeny-receiver`
    dependency). `brightness_cap` (SET_BRIGHTNESS 255, expect `< 255`) needed
    no change.
  - `timeout 300 cargo test -p screeny-probe -p screeny-sim`: all green
    (probe: 15 unit tests + fixtures; sim: every integration test file,
    including `http_routes.rs`'s `settings_are_clamped_by_the_same_code_udp_clamps_with`).
- Step 4: spec. The card says "section 6.6"; in the spec as it stands today
  `SET_BRIGHTNESS`'s own text is a bullet under **§6.3** (Opcodes) - §6.6 is
  `GET_INFO`'s reply body and has no brightness prose of its own (only the
  telemetry byte's one-line table entry). Wrote the new paragraph into §6.3,
  after the existing "`SET_BRIGHTNESS`... `applied`... how a sender learns the
  cap" bullet: the 25-real-steps fact, the `1..=5` off-by-duty floor, the
  snap-up-to-the-lowest-lit-level rule, 0 always off, and the cap-wins case
  when the cap itself sits below the floor.
  §8.6 (`POST /api/v1/settings`)'s bullet already said the reply is "the whole
  settings state after clamping, not an echo" - extended one clause to say a
  caller whose brightness was raised to the dimmest lit level learns that
  value too, same as a caller whose brightness was capped.
  Checked `docs/design/device-web.md` (no brightness-resolution prose there,
  just the `POST /api/v1/settings` route-table row - unchanged) and
  `docs/design/generative-art-brief.md` (its brightness row already says
  "25 real steps" - card 020/066 got there first, nothing to fix) and
  `docs/design/architecture.md` (no brightness-resolution claims).
