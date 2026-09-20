---
id: 195
title: Studio device health - thresholds the firmware session measured, and "it may have crashed"
type: build
hardware: no
depends: [180, 192]
owner: worker-195
branch: card/195-device-health-thresholds
---

## Goal

The Device block (card 180) warns when the panel is actually in trouble, using numbers from
the people who measured the panel, and says honestly when a reboot may have been a crash.

## Context

Card 180 chose its thresholds from two readings. The firmware session, which measured the
real device across several firmware builds, replied (2026-09-20):

1. **Stack: `low_stack < 2048` is too late.** Interrupts land on core 0's stack at 256 bytes
   of context per level, and `stack_free` is a high-water mark that only ever falls. Healthy
   0.4.3 reads ~17-20 KB; the 0.4.0 build that worried them read 4-5 KB. **Warn below 8192,
   fault below 4096.**
2. **Heap: > 85% is a good fault line.** Steady state 45.6 KB of 90 KB (51%); the measured
   worst instant, with the setup AP up, is 54 KB (60%).
3. **Quiet resets** (`power_on`, `software`, `external`) are right, with a caveat: the ESP32
   cannot tell a panic from any other software reset, so today `software` also covers a
   crash-and-restart (a later firmware card adds an RTC breadcrumb so `panic` becomes
   reportable). Until then the honest "it may have crashed" signal is **a change of `boot_id`
   with `reset_reason: software` that the Studio did not ask for**.

The firmware session's card 192 (in flight on their side, `crates/sim`) adds flags and
`SimHandle::set_health(..)` so a simulator can play an unhealthy device; card 180's
acceptance screenshot of the stand-out rows against a device-shaped thing is still owed and
belongs here.

## Deliverables

- Two levels for the stack (`warn` < 8192, `fault` < 4096), one for the heap (> 85%), as
  named flags in `DeviceFacts` - the page still carries no copy of any number.
- "Unexpected reboot": the Studio knows when *it* asked for a reboot (its own reboot
  control). A `boot_id` change with `reset_reason: software` that it did not ask for is
  counted and shown separately from reboots in general ("1 reboot the studio did not ask
  for - it may have crashed"), quiet wording, not a fault: on this bench a reflash looks the
  same, and the page should say so rather than cry wolf. A comment in the code records why
  `software` cannot be trusted to mean "clean".
- Against the simulator with card 192's controls: each stand-out row seen on the page, in a
  real browser, screenshots in the Log (dummy SSID only - never the deployed page).
- No state schema change; additive API only.

## Acceptance

A simulator flipped to stack_free 6000 shows a warning, 3000 a fault, heap 90% a fault, a
brownout reset stands out, and an unasked-for software reboot is named as such; the real
panel at ~20 KB free and 51% heap shows nothing but ordinary rows.

## Log

### 2026-09-20, worker-195, branch `card/195-device-health-thresholds`

Claimed as written. The numbers here are the firmware session's, measured on the real
device; nothing below second-guesses them. Everything was built and shown against
`screeny-sim` on loopback: **no hardware, no LAN**, and neither the bench panel nor the
live service on `workbench.local` was touched.

**The thresholds, in one place** (`crates/studio/src/devices.rs`, beside the reasoning):
`STACK_WARN = 8192`, `STACK_FAULT = 4096`, `HIGH_HEAP = 0.85`. The comments carry *why*
rather than only *what*: 256 bytes of interrupt context per level, `stack_free` being a
high-water mark that only ever falls, healthy 0.4.3 at 17-20 KB and the 0.4.0 build that
worried them at 4-5 KB; the heap's steady state of 51% and its measured worst instant of
60% with the setup AP up. Card 180's `LOW_STACK = 2048` is now `= STACK_FAULT`, and the
comment says what went wrong: it was drawn from two readings, one of which (`stack_free
4312`) turned out to be the *unhealthy* build.

`DeviceFacts` gained `stack_warn`, `stack_fault` and `unasked_reboots`. **`low_stack`
stays**, as an alias of the fault level: `/api/v1/status` is additive and a published
field is not removed, even though the page is the only reader anybody knows of (checked:
`main.js`, `tests/device_status.rs`, `tests/ui.rs`, and nothing else in the tree). A
device below the fault line is below the warning line too - `stack_fault` implies
`stack_warn` - so the page reads the fault first and the two can never disagree.

**"A reboot the studio did not ask for."** The studio asks for exactly one kind of
reboot, its own control (`POST /api/v1/device/reboot`), and now writes the ask down:
`Registry::asked_to_reboot(id)` stores an `Instant` on the record - live only, never
persisted, never on the wire, because a studio that restarts has honestly asked for
nothing. A `boot_id` change **consumes** the ask, whether or not it was still fresh, so
one ask excuses exactly one reboot and a stale ask cannot sit there waiting to excuse
next month's crash. A change with no fresh ask behind it and `reset_reason: software`
counts in `unasked_reboots`.

- **The window is two minutes** (`REBOOT_ASK_WINDOW`), and it is not a guess: the panel
  is back in a few seconds but the *studio* does not look that often. The status poll is
  `MIN_DEVICE_HTTP_EVERY` (10 s) and a read that fails while the panel is down backs the
  next one off, up to `MAX_BACKOFF` of twelve passes - so two minutes is the longest the
  studio can go between reads, and therefore exactly long enough to cover the first read
  that can possibly see the reboot. Anything later we would rather count than excuse.
- **The ask is recorded before the request goes out**, not after it succeeds: a panel
  that received `REBOOT` and restarted before its acknowledgement got back has still done
  what we asked. The cost of the other mistake - an ask that reached nobody - is that one
  crash in the next two minutes is not named. Crying wolf is the expensive error here.
- **Why `software` cannot be trusted**, written into the code beside `QUIET_RESETS`: the
  ESP32 cannot tell a panic from any other software reset, so `software` covers our own
  `REBOOT`, a reflash *and* a crash-and-restart. It stays a quiet reason (no fault tone,
  no `odd_reset`), and the honest signal is the ask, not the reason. When the firmware
  reports panics, this gets simpler.

**The page** (`showDevice` and one new helper, `rebootLine`; no CSS added, no WebSocket
code touched - card 120's worker is in that file). Free stack is `bad` below the fault
line and `warn` below the warning one; memory past 85% is now `bad` rather than `warn`,
which is what "a good fault line" means. The Reboots row reads *"2 since the studio
started · 1 the studio did not ask for"*, **with no tone at all**, and one note under the
block says the only honest thing there is to say: *"A crash and a reflash look the same
from here."* It is never a fault and never reaches `/healthz`. The page still carries no
copy of any threshold; `tests/ui.rs` now slices `showDevice` and its helper properly
rather than by a byte count, forbids every number that has ever been a threshold (2048,
4096, 8192, 0.85), counts the four fault tones, and checks the unasked-for reboot is said
without one.

One line on the studio's log when it happens, beside card 180's:
`studio: `Bench panel` rebooted without the studio asking: 1 of 2 so far`.

**Tests.** `crates/studio/tests/device_health.rs`, 7 against a **running** simulator with
`SimHandle::set_health` (card 192) - no respawn, and the poll is 300 ms through the
library `Config`, which is the trick card 180's tests left behind; the product's ten-second
floor is untouched and `main.rs` still holds it.

- a stack of 6000 warns and does not fault; 3000 faults (and `low_stack` follows the
  fault level); 20272 is an ordinary row again;
- a heap at 90% faults, and the 60% the firmware session measured with the setup AP up
  does not;
- a brownout stands out **and is not a reboot anybody counted** - `boot_id` never moved;
- three store errors and a `pending_verify` slot stand out, and neither is a *server*
  fault: `/healthz` stays 200;
- a reboot asked for through the studio's own route is counted as a reboot and **not** as
  unasked, the ask is used up, and the next one taken behind the studio's back is counted;
- a simulator rebooted over its control port directly is named as unasked-for;
- the simulator's defaults and **the real panel's readings** (`stack_free 20272`,
  `45612/90112`) show nothing but ordinary rows.

Every wait is for a read that *began after* the change and carries all seven of the
simulator's health numbers. Waiting on the values alone was not enough and the test said
so: a brownout changes nothing but the reason, so a stale read satisfied the wait. That
is `Bench::health`'s `reads >= before + 2` plus a field-by-field compare.

The **window** is a unit test rather than a two-minute wait (`devices.rs`): one ask
excuses one reboot and is consumed; an ask older than the window excuses nothing and is
cleared rather than left lying around; a brownout is not an unasked-for *software*
reboot; and the registry's half - one panel's ask is not another's.

**Rendered in a real browser**, Chrome, against simulators on loopback only. Nothing
touched the bench device or `workbench.local`; the only network name in any picture is
the simulator's dummy, `simulated`.

- `docs/research/img/195-fault-rows.png` - card 192's acceptance command plus a 90% heap
  and a `pending_verify` slot: Memory, Free stack, Last reset and Store errors in the
  fault tone, Slot in the warning tone, everything else ordinary. Appended to card 192's
  Log as its closing evidence, and that card is now in `done/`.
- `docs/research/img/195-warn-and-asked-reboot.png` - `--stack-free 6000`: Free stack in
  the **warning** tone, and after a reboot asked for through the studio's own route,
  *"Reboots 1 since the studio started"* with no note and no tone. The Reboot button
  itself was not clicked: it goes through `window.confirm`, which would block the browser
  extension, so the route it calls was called directly.
- `docs/research/img/195-unasked-reboot.png` - then `POST /api/v1/reboot` on the
  simulator itself, behind the studio's back: *"2 since the studio started · 1 the studio
  did not ask for"* and the note under the block.
- `docs/research/img/195-narrow-390.png` - the same at **390 px**, in a same-origin
  iframe of exactly that width (the extension's window will not shrink that far):
  `innerWidth 390`, `scrollWidth 390`, no horizontal overflow, the long Reboots value
  wrapping onto a second line inside its column.

Console clean on every load, watched from before navigation. Tabs closed. Every simulator
and studio was started under `timeout` with `--exit-after`, and all of them are stopped.

**`crates/studio/README.md`** gained "What counts as trouble, and what only looks like
it": the thresholds and whose numbers they are, `low_stack`'s new meaning, the ask, the
window and why it is that length, and the new test file in the table.

**No new cards.** Nothing was found that belongs outside this one; 187-189 are unused.

**Green.** Root `cargo test --release --no-fail-fast`: **711 passed, 0 failed** across every
crate (was 685 at card 180's merge). `cargo clippy --workspace --all-targets`: silent, no
new `#[allow]` anywhere. `crates/studio` on its own after the last commit: 128 passed, 0
failed. `tests/ssid.rs` unchanged but for the one call that gained a parameter, and
passing. `ps` shows nothing of mine running.
