---
id: 230
title: Firmware - the button: a short press shows the status screen, holding 5 s wipes WiFi and opens the setup portal
type: build
hardware: orchestrator flashes; the owner presses (the worker builds and host-tests only)
depends: [202, 223, 243]
owner: opus worker (firmware session, 2026-09-21)
branch: card/230-the-button
---

## Goal

The owner's second ask for the device-web track ("a use for the Tidbyt's button,
specifically resetting WiFi"), in the one-card shape he chose on 2026-09-20 (decision 10 in
`docs/design/device-web.md`, which narrows decision 8):

- **Short press** (released before 1 s): the status/identify screen - IP address, name,
  RSSI, firmware version - for **10 s**, then back to whatever was showing. A press during
  a stream overlays it; the stream keeps being decoded underneath (as `IDENTIFY` does).
- **Hold**: past **1 s** an on-panel **countdown** to 5 s starts ("hold to reset WiFi" and
  the seconds left); **releasing before 5 s cancels** and changes nothing. At **5 s** the
  stored WiFi credentials are wiped and the setup portal comes up (QR screen).
- **Not in this card** (dropped by decision 10): the 15 s factory reset, held-at-boot.

## Context (read first)

- `docs/research/008-button.md` and the done card `docs/board/done/202-research-button-gpio.md`
  (the bench confirmation, "card 203" in `docs/design/device-web.md`'s table, is recorded in
  those two): the button is **GPIO15, active low**, confirmed with the owner pressing; note
  008's point 4 - **GPIO15 is dual-purpose on this board** (it is also an ADC input for the
  light sensor path): read it before configuring the pin. `firmware/src/bin/gpio_probe.rs` is the probe that found it.
- `crates/provision`: the machine already has `Event::ButtonWipe` (-> `StopJoin`,
  `ClearCredentials`, `RaiseAp`; host-tested from every state, including during an
  online-origin trial). Wire the real button to it; do not invent a second path. Credentials
  are cleared through the settings store the way the machine's action already does it
  (`firmware/src/provision.rs` applies `ClearCredentials`).
- `firmware/src/screens.rs` (`identify`, the idle/status screen), `firmware/src/net.rs`
  (how overlays are composed: `Intent::Identify`, card 223's setup-screen overlay, the rule
  that a streamed frame is decoded but not shown under an overlay), `crates/receiver`
  (`identify_until_us` - reuse the mechanism rather than adding a parallel one if it fits).
- The rules every firmware card lives by (`docs/design/device-web.md`, "How to think about
  storage and RAM" + the lessons): `.bss` is core 0's stack (`tools/fw-size.sh`, `.stack`
  >= 24,576; 26,200 on fw 0.7.0); nothing large across an `await`; **flash writes only
  through `store::guarded`** (card 245) and never inside an interrupt or an HTTP handler;
  the always-on 20 s liveness watchdog (card 241b) must not be starved by a busy-wait;
  decision 7 - the frame path is the product: a GPIO interrupt or a 20 ms poll is fine, a
  task that spins is not.
- The panel runs off laptop USB: no full-white screens, nothing flashing above 3 Hz
  (CLAUDE.md). A countdown that redraws once a second is fine.
- During an OTA **trial** or an upload, a WiFi wipe would sabotage the health check /
  orphan the upload: decide what the button does then (ignore the hold, show why) and say so.

## Steps

1. The gesture recogniser as a pure, host-tested state machine (in `crates/provision` or a
   small sibling - one implementation, `no_std`): input = (level, now_ms), output = events
   `ShortPress`, `HoldStarted`, `HoldTick(seconds_left)`, `HoldCancelled`, `WipeWifi`.
   Debounce from 008's measurements; `now_ms` wraps (u32) and every comparison is wrapping.
   Tests: bounce, a press of 999 ms vs 1,000 ms, release at 4.9 s vs hold to 5.0 s, a press
   that straddles the wrap, a stuck-low pin at boot (must not wipe: require a release
   before the first press counts), repeated presses during the 10 s screen.
2. The firmware task: GPIO15 configured per 008, fed into the recogniser; events applied -
   status overlay for 10 s, the countdown screen, `Event::ButtonWipe` into the machine.
   The countdown and status screens live beside the other screens (one renderer).
3. The simulator: a way to press the button (`crates/sim` API or CLI flag) so the portal
   flow can be driven end to end on the host; a test that a 5 s hold on an online simulated
   device ends at the portal with credentials cleared, and a 4 s hold changes nothing.
4. Spec: the button's behaviour in `docs/design/protocol-v1.md` section 8 (it is how a user
   reaches 8.1's portal on purpose), three sentences. `docs/design/device-web.md`: the
   owner-facing paragraph.
5. `FW_VERSION` -> next minor after whatever `main` has.

## Exit

All firmware builds over the `fw-size.sh` floor, numbers in the Log; `timeout 1200 cargo
test` green; the ELF in the orchestrator's scratchpad; and a **bench procedure for the
orchestrator and the owner** (he must be at the panel): short press -> status 10 s (under a
live stream too); hold 3 s and release -> countdown appears, cancels, nothing changed
(`GET /api/v1/wifi` still connected); hold 5 s -> portal comes up, QR on the panel,
`screeny-4a00a4` network visible; then re-provision from the phone (card 223's flow) or by
`bench-wifi` build if he prefers - **say which and how before the wipe**, because after it
the panel is off the LAN until someone gives it credentials again.

## Rules

Branch `card/230-the-button` from current `main`, own worktree; commit and Log as you go by
explicit path; do not merge or push. No hardware, no serial port, no LAN, no camera.
Every command bounded with `timeout`, **every wait written as
`timeout N bash -c 'until ...; do sleep S; done'`**, and before the final report run
`ps -axo pid,ppid,etime,command | grep -E 'sleep|until|timeout'` and kill what is yours.
Never write a real SSID or password (dummies `Example-Wifi1` / `password9`). Leave
`crates/art` and `crates/studio` alone; list every change to shared crates.

## Log

### 2026-09-21 - step 1: the recogniser (`crates/provision::button`)

Branch `card/230-the-button` from `main` at 4bc39c2.

- The recogniser is `crates/provision/src/button.rs`: `Recognizer::poll(level,
  now_ms, wipe_allowed) -> ButtonEvent`, plus `active()` so the firmware task can
  sleep on the pin's interrupt when nothing is in flight. No clock, no I/O, no
  allocation; every comparison wrapping. Constants: `DEBOUNCE_MS` 30, `HOLD_MS`
  1,000, `WIPE_MS` 5,000, `STATUS_MS` 10,000.
- **Durations are timed from the edge, not from the poll that noticed it.** The
  first version timed from the settled poll, which put the debounce window and
  the scheduler's latency inside every threshold - a 1,000 ms press read as
  900 ms. `raw_since` (the instant the pin moved) is the timestamp now, so
  "999 ms or 1,000 ms?" is answered by the button.
- **A pin low at boot is ignored until it has been released** (`Phase::WaitRelease`,
  and it takes *both* readings high to leave it). A jammed switch cannot wipe
  anything, and the release that follows it is not a short press either.
- **The countdown reads 4, 3, 2, 1** - the hold starts at 1 s and fires at 5 s,
  so there are four seconds to show. `seconds_left` is whole seconds remaining,
  rounded up.
- **A late poll still shows the countdown before it wipes**: a poll that crosses
  both thresholds at once starts the countdown and lets the next poll fire, so
  the panel has always said what was about to happen.
- **One press, one wipe.** After `WipeWifi` the recogniser waits for a release;
  leaning on the button does not wipe twice.
- **OTA decision (the card's open question), first half:** `poll` takes
  `wipe_allowed`, read *both* when the countdown would start and again when the
  wipe would fire. False -> `HoldRefused`, no countdown, no wipe, and the
  gesture is over until the button comes up. So an upload that starts during a
  countdown still stops the wipe. A short press is unaffected: showing a status
  screen costs an update nothing.
- Tests: `crates/provision/tests/button.rs`, 12 of them, every case the card
  names (bounce incl. the 11 ms release glitch card 203 photographed, 999 vs
  1,000 ms, 4.9 s vs 5.0 s, the u32 wrap, stuck-low at boot, four presses inside
  one status screen) plus the two OTA cases and the late poll.
  `cargo test -p screeny-provision`: **47 passed, 0 failed**, 2 doc-tests.

### step 2a: the three button screens (`crates/provision::screen`)

`Screen` gains `WipeCountdown { seconds_left }`, `WipeCancelled` and
`WipeUnavailable`, drawn by the same renderer as the portal and the updating
screens - which is what the card means by "one renderer": the firmware and the
simulator draw the same pixels, and the tests are host tests.

- The countdown: "wipe wifi" in amber, the digit beside it, "keep holding" /
  "let go = keep", and four blocks along the bottom that go **out** one a
  second. Blocks rather than a shrinking bar because "three blocks" is readable
  across a room; going out rather than filling up because a progress bar
  suggests something is being built. It redraws once a second (CLAUDE.md:
  nothing above 3 Hz), and the lit area is a fraction of the panel.
- `WipeCancelled` ("cancelled / wifi kept") exists so that letting go has an
  answer; without it a cancelled hold and a hold that never started look the
  same.
- `WipeUnavailable` ("wifi reset / not while / updating") is the OTA refusal.
- Tests: a block goes out per second, the four countdown frames are four
  different pictures, the three screens are told apart and none is blank or
  bright, and all of them join the existing "nothing renders a full white
  frame" list. `cargo test -p screeny-provision`: 25 + 12 + 8 + 47 passed.

### step 2: the firmware task (`firmware/src/button.rs`)

One task on core 0, `button_task(AnyPin)`, spawned after the radio and the
store (a wipe needs both). It owns GPIO15 and nothing else touches that pin.

- **The pin, and 008 point 4.** `Input::new(pin, InputConfig::default()
  .with_pull(Pull::Up))` - exactly what the stock firmware's `gpio_config_t`
  says, and what card 203 confirmed on this unit. The module header carries the
  three consequences of GPIO15 being dual-purpose: nothing may ADC-read the
  board-ID strap while this task holds the pin (they are exclusive, and a
  revision read would have to happen once, before the spawn, tolerating a held
  button reading ~0 mV); the internal pull-up is belt and braces rather than
  the only thing holding the pin up (phase B of the probe rested high with the
  internal *pull-down* selected, so the strap network holds it); and **a held
  button at boot silences the ROM boot log**, because GPIO15 is MTDO - harmless,
  logged as a warning at boot so nobody spends an hour on it. It is not GPIO0,
  so a held button at boot cannot strand the device in the ROM bootloader.
- **What it costs the frame path** (decision 7): the task sleeps in
  `Input::wait_for_any_edge()` - interrupt-backed; esp-hal binds a default GPIO
  handler in `esp_hal::init`, so there is nothing to install - raced against a
  timer: 1 s idle (a backstop, so a missed edge cannot mean a dead button until
  the next reboot) and 50 ms while a gesture is in flight. An untouched button
  is one wake a second. Nothing spins.
- **Overlay composition** in `net.rs`, in rank order: **OTA > button > portal >
  the bench pattern hold > `Intent`**. The button screens join `setup_screen_up`,
  so a streamed frame underneath is still drained, decoded and counted and only
  the panel is taken - exactly what the portal screen does. They sit below the
  update screen because "updating - do not unplug" is a better answer to a
  refused hold than the refusal screen is, and above the portal so that holding
  the button while the portal is up still shows what is about to happen. A new
  `button_edge` joins `ota_edge` in the redraw test, so the countdown redraws
  the instant its number changes and the transient screens end on time.
- **The short press is `IDENTIFY` for 10 s**, raised through the one receive
  core as a `req_id` 0 request (spec 6.1's "no reply wanted") - the mechanism
  the card asked me to reuse rather than a second overlay timer. So the
  telemetry byte reads `IDENTIFY`, a press during a stream overlays it with the
  stream still decoded underneath, and a second press restarts the ten seconds.
  `screens::identify` gains a third line, `fw <version> <rssi>`: spec 6.3 asks
  for "a high-contrast pattern plus the device name and IP" and that is still
  what it is, but the owner standing at the panel with no laptop is who the
  button is for.
  **This cost 864 bytes of core 0's stack when it was written as a call to
  `http::apply_control`**, which does the same thing and then awaits a flash
  write for the opcodes that need one: awaiting that future from this task put
  the whole settings-write call chain into the task's `.bss`. Written out (a
  16-byte request buffer, the `CORE` lock, one `control` call) the task's future
  is **216 bytes** instead of 1,080. Nothing large across an `await`, as the
  design file says.
- **The wipe** is `crate::provision::BUTTON_WIPE.signal(())`, a fourth arm on
  `provision_task`'s `select`, which feeds `Event::ButtonWipe` to the one
  `Provisioner` and carries out what comes back. `Action::ClearCredentials` is
  implemented at last (it has been an unreachable `warn!` since card 223):
  `store::clear_wifi()` - a `with_store!` write like any other, so it is counted
  and goes through `store::guarded` (card 245) - then the in-RAM `stored` and
  `trial` copies are dropped and `set_current_ssid` is emptied, or the next
  `StartJoin` would cheerfully rejoin the network the owner just asked the
  device to forget. **No flash is written from the button task, from an
  interrupt or from an HTTP handler.**
- **OTA decision, second half.** `wipe_allowed()` is
  `!ota::updating() && !ota::activating() && !ota::trial_pending()`, where
  `trial_pending()` is new in `ota.rs` and means "on trial and not yet
  confirmed" - `boot_class()` alone would have refused every wipe until the next
  reboot after any update. A refused hold shows "wifi reset / not while /
  updating" for 2.5 s and is logged; during an upload the updating screen
  outranks it and says the same thing better. Short presses still work.
- `fw-size.sh` after this step: `.stack` **25,800** (floor 24,576; 26,200 on
  fw 0.7.0), `.bss` 110,696, image 1,026,741. The whole card costs 400 bytes of
  core 0's stack. `cargo clippy --release` in `firmware/`: no new warnings in
  any file this card touches.

### step 3: the button on a host (`crates/sim`)

`crates/sim/src/button.rs` (new, 120 lines) drives **the firmware's
recogniser** with synthetic timestamps, and `SimHandle::press_button(hold_ms)`
plays a whole press: down, held, released, debounced. The clock is the
recogniser's own and is advanced only by a press, so `press_button(5_000)`
costs a test microseconds instead of five seconds; the screens it raises do
expire on the simulator's real clock, because a window may be showing them.

- `Core::press_button` carries the gestures out the way the firmware does: a
  short press is an `IDENTIFY` through the same control path (`req_id` 0), five
  seconds is `WifiModel::wipe`, which is the `Event::ButtonWipe` the machine
  has had since card 221. The screens go into the scene **above** the portal's,
  as they do in `net.rs`.
- The diff is deliberately small and local: one new file, one field and two
  methods on `Core`, one method on `SimHandle`, one line in `render_display`.
  Card 246 is in `crates/sim` at the same time.
- `wipe_allowed` is `true` in the simulator: its `POST /api/v1/firmware` does
  not claim the device the way the firmware's does, and card 246 is changing
  that area. The gate itself is host-tested in `crates/provision/tests/button.rs`.
- `crates/sim/tests/button.rs` (new, 7 tests): a 5 s hold ends at the portal
  with the credentials cleared and `GET_WIFI` reading `disconnected` (a wipe is
  not a failure); a 4 s hold and a 4.95 s hold change nothing at all; a short
  press raises the overlay (telemetry byte `IDENTIFY`) and moves nothing else;
  four presses in a row keep it up; a cancelled hold leaves *the* "cancelled"
  screen on the panel, compared pixel for pixel against
  `screeny_provision::render`; a wipe leaves the portal screen with its QR.
  `cargo test -p screeny-sim`: **all suites green**, 7 new.
