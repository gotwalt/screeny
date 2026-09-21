---
id: 230
title: Firmware - the button: a short press shows the status screen, holding 5 s wipes WiFi and opens the setup portal
type: build
hardware: orchestrator flashes; the owner presses (the worker builds and host-tests only)
depends: [202, 223, 243]
owner:
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
