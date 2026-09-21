---
id: 247
title: Firmware - two things the button bench found: the status screen flickers over a live stream, and the portal QR no longer scans
type: build
hardware: orchestrator flashes; the owner looks (the worker builds and host-tests only)
depends: [230, 223]
owner: opus worker (firmware session, 2026-09-21)
branch: card/247-button-bench-findings
---

## Goal

Card 230's bench passed on the device (fw 0.8.0/0.8.1, 2026-09-21: short press at idle,
hold 3 s and release, hold 5 s -> portal -> re-provisioned from the owner's iPhone ->
rejoined after a reboot). The owner saw two things by eye. Fix both, nothing more
(decision 10 in `docs/design/device-web.md`).

## The two, as observed on the device

1. **The status (identify) screen flickers when a stream is live: "the art flickers
   through".** Seen on a short button press over the Studio's 30 fps stream, and then
   reproduced **without the button**: `screeny identify --addr 192.168.7.221 --ms 10000`
   over the same stream flickers the same way. So it is the `IDENTIFY` overlay itself
   (older than card 230 - nobody had looked at it by eye under a live stream), and the
   button's short press inherits it because it reuses that path. At idle the screen is
   solid. The button's *other* screens (countdown, cancelled - the `setup_screen_up`
   route, where a streamed frame is decoded and counted but not shown) did **not**
   flicker over the same stream: that route is the model. Device-side numbers during
   all of this were clean (LIVE, 30.0 fps, 0 dropped, 0 rejected, no panic).
   Find where a presented frame still reaches the panel while `Intent::Identify` is up
   (`firmware/src/net.rs`, the display task / swap path, `crates/receiver`'s
   `identify_until_us` and what it tells the caller to show) and make an identify overlay
   own the panel the way the setup screens do: frames keep being received, decoded and
   counted (spec 6.3 / decision 7), none is shown until the overlay ends, and the picture
   returns on the next frame after it. Check the simulator does the same thing the
   firmware does (one implementation in `crates/receiver` if the rule can live there).
2. **The portal's WiFi QR code is not detected by the iPhone camera.** Decision 1 in
   `device-web.md` records the same phone scanning it "easily" on 2026-09-20 (version 2-L,
   25x25, one LED per module, lit white background with dark modules off, 3-pixel lit
   quiet zone, **default brightness**), and card 223 later made the QR "stay put". Today
   the owner joined the network by hand instead. Known difference today: the runtime
   brightness was **56** (set by the Studio; it was the default when decision 1 was
   measured), and screens go through the brightness path (`firmware/src/screens.rs`
   header: status screens honour `BRIGHTNESS`). The panel dims by shortening the
   output-enable window (25 slots), which a phone's rolling shutter sees as banding - a
   dim QR may be unreadable to a camera while fine to the eye. Work out, from the code
   and the git history since card 223 / decision 1, what changed about how the QR is
   presented: brightness applied to it, the quiet zone, polarity, position, anything
   redrawing or alternating on that screen (card 223's "connected screen yields",
   the button overlay rank added by card 230, a countdown/cancelled screen left over the
   portal after the 5 s hold). Then make the portal screen scannable by construction:
   the QR screen is shown at a fixed, known-good brightness regardless of the runtime
   setting (restore the setting when the portal ends; never above the firmware cap, never
   a full-white frame beyond what decision 1 already measured), and nothing else draws
   over or alternates with it. Say in the Log what you found and what you could not
   determine without the panel.

## Exit

`FW_VERSION` -> "0.8.2" (0.8.1 was used on the bench as an upload image; skip it). All
firmware builds over the `tools/fw-size.sh` floor (`.stack` >= 24,576; 25,800 on 0.8.0),
numbers in the Log; `timeout 1200 cargo test` green; host tests for 1 (a frame arriving
during identify is counted and not shown; the first frame after it is shown) in
`crates/receiver` and/or `crates/sim`, and for 2 whatever is host-testable (the portal
screen's brightness rule as a pure function; the QR bitmap unchanged from decision 1's
geometry). Artefacts in the orchestrator's scratchpad (path in the worker prompt): the
flash ELF and an OTA upload image (`espflash save-image ...` as card 240's Log says).
Bench procedure for the orchestrator and the owner: identify over a live stream is solid;
`start-in-portal` build or a 5 s hold shows a QR the iPhone camera detects.

## Rules

Branch `card/247-button-bench-findings` from current `main`, own worktree; commit and Log
as you go by explicit path; do not merge or push. No hardware, no serial port, no LAN, no
camera. Every command bounded with `timeout`, every wait written as
`timeout N bash -c 'until ...; do sleep S; done'`, and before the final report run
`ps -axo pid,ppid,etime,command | grep -E 'sleep|until|timeout'` and kill what is yours,
by pid. Never write a real SSID or password (dummies `Example-Wifi1` / `password9`);
never quote lines from `captures/`. Leave `crates/art` and `crates/studio` alone; list
every change to shared crates (`receiver`, `proto`, `sim`, the spec).

## Log

### 2026-09-21, worker on `card/247-button-bench-findings` (branched from `main` at 0ec0795)

**Baseline before any change** (fw 0.8.1 tree, `tools/fw-size.sh`): `.data` 60,108,
`.bss` 110,696, **`.stack` 25,800** (floor 24,576), `.rwtext` 67,740, image 1,027,829.

#### Step 1 - item 1, the root cause of the identify flicker, found and named

It is `firmware/src/net.rs`, the frame task, and it is a **publish race**, not a
drawing bug. The task had this (0.8.1, `net.rs:305-316`):

```rust
let setup_screen_up = ota.is_some() || button.is_some()
    || matches!(portal, Some(PanelScreen::Portal { .. }));
if published {
    last.copy_from(producer.back());
    if !setup_screen_up { producer.publish(); }   // <-- the streamed frame goes up
}
...
if due { ... Intent::Identify => screens::identify(...); producer.publish(); }
```

`setup_screen_up` lists every screen that owns the panel **except the identify
overlay**. So while `Intent::Identify` is up and a sender is streaming, each pass of
the loop publishes *twice*: first the decoded frame, then - a few hundred
microseconds later, after `net_state`, `tick`, `intent` and a full
`screens::identify` draw - the status screen. The two land in different slots of the
lock-free triple buffer (`firmware/src/fb.rs`), and core 1 latches whatever is
published ~154 times a second (6.5 ms refresh period). Any refresh that falls inside
that window scans out the **art** for a whole refresh. At 30 fps that is 30 chances a
second at roughly a 5-10% hit rate: a handful of frames of the picture punching
through the status screen every second - exactly "the art flickers through", and
exactly why it is invisible at idle (no frame, no first publish) and invisible for the
countdown/cancelled/portal screens (they are in `setup_screen_up`).

It is older than card 230, as the card says: the button's short press only inherits
`IDENTIFY`, and `screeny identify --ms 10000` over the same stream reproduces it. The
`screens::identify` screen is fully opaque (it `clear()`s and redraws every pixel), so
nothing about the drawing lets the art through - only the extra publish does.

**The rule now lives in `crates/receiver`**, where there is one of it:
`Intent::shows_frames()` (`crates/receiver/src/lib.rs:367`) - "a frame decoded this
instant may reach the panel" - is `false` for `Intent::Identify` and `true` for
`Stream`, `Fade` and `Idle`. New host tests, `crates/receiver/tests/identify_overlay.rs`
(4 tests): a frame arriving under the overlay is decoded into the caller's buffer and
counted in `frames_shown` while `shows_frames()` is false, the stream state is
untouched, the overlay expires on its own tick and `shows_frames()` is true again for
the next frame, and `IDENTIFY 0` stops it.

`cargo test -p screeny-receiver`: 19 tests, all green.

#### Step 2 - item 1 in the firmware

`firmware/src/net.rs`:

* the timers (`link_down`, `tick`) and `core.intent(now)` move **above** the swap.
  They have to: the answer to "may this frame reach the panel?" is `intent`, and an
  `intent` read before `tick` is one tick stale about an overlay that has just run
  out. Nothing there touches the producer, so it costs the frame path nothing -
  `tick` cannot release a lock a frame has just renewed, and `link_down` only turns
  `Live` into `Hold`, which shows frames either way.
* `setup_screen_up` becomes `overlay_owns_panel` and gains `|| !intent.shows_frames()`
  (`net.rs:339-343`). That one clause is the fix.
* `screens::identify` now takes `now_ms` instead of `phase`
  (`firmware/src/screens.rs:190-203`). Its chevron border was `(phase / 3) % 2`, and
  `phase` advances once per redraw of the frame task: 50 a second at idle and up to 80
  with a stream arriving, so the border was alternating at **8-13 Hz** - over
  CLAUDE.md's 3 Hz panel limit, and at a *different rate* depending on whether
  anything was streaming, which is the other half of "it flickers over a stream and is
  solid at idle". It is now a 400 ms wall clock (1.25 Hz) that does not change when a
  stream starts or stops.

`crates/sim`: the simulator composes the whole scene on every render rather than
swapping buffers, so it never had the gap the firmware had - it was already right.
`crates/sim/tests/arbitration.rs::identify_is_an_overlay_and_not_a_state` now pins
that it stays right: not one pixel of the streamed frame reaches the panel while the
overlay is up, and the picture is the panel's again the moment it ends.

**One firmware/simulator divergence found and deliberately left alone**: in the
simulator `identify_overlay` draws *over* the portal screen (`crates/sim/src/screens.rs:146`),
while in the firmware the portal branch outranks `Intent::Identify` and identify never
draws over a QR. The firmware's order is the one card 247 item 2 wants ("nothing else
draws over or alternates with it"), and nothing on the device can raise identify while
the portal is up anyway - the portal means no LAN, and the button's short press is the
only other route. Noted for a later card rather than changed here.

Build after this step: `.stack` **25,800** (unchanged from the 0.8.1 baseline; floor
24,576), image 1,027,841. `cargo test -p screeny-sim --test arbitration`: 9 green.

#### Step 3 - item 2, the QR

**What changed since decision 1 / card 223, read out of the code and the history:**

* The QR bitmap has **not** changed. `crates/provision/src/screen.rs`'s drawing of
  `Screen::Portal { layout: QrAndName }` - `blit_qr`, `QUIET = 3`, `QR_X = 3`,
  `QR_Y = (32-25)/2 = 3`, lit white background with dark modules off, one LED per
  module - is untouched since `2dae13a` (2026-09-20, "the QR stays on the panel",
  fw 0.5.1), which is the build the owner's phone scanned. The only commit to touch
  that file since is `f7e6d5c` (card 230), and it only *adds* three button screens.
  `uri.rs` and `qr.rs` have not been touched since card 221. `crates/provision`'s
  existing tests already prove the bitmap independently: `rqrr` (a decoder that has
  never seen our encoder) reads `WIFI:T:nopass;S:screeny-4a00a4;;` back off the frame,
  and an FNV-1a golden hash pins all 6,144 bytes.
* The alternating layouts are gone (card 223) and nothing has brought them back. The
  portal screen is recomposed every `PORTAL_MS` = 250 ms with identical pixels, so it
  does not move or blink.
* Nothing draws over it on the device. The frame task's rank is update > button >
  portal, and the button's countdown/cancelled/refused screens all clear themselves -
  `WipeWifi` calls `clear()` before signalling the wipe, so no button screen survives
  into the portal. `Intent::Identify` cannot draw over the portal either: the portal
  branch is tested first. (The *simulator* does draw identify over the portal; see
  step 2's note.)
* **The one thing that did change is the brightness.** The runtime setting was 56
  (the Studio sets it); decision 1 was measured at the default, 96. This panel dims by
  shortening the output-enable window, not by scaling pixels:
  `display::slots_for(56)` = **5** of 25 slots, `slots_for(96)` = **9**. A little over
  half the light, and a phone's rolling shutter sees a short OE window as banding
  across the code - which is exactly the failure mode where a QR looks fine to an eye
  and will not scan.

**The fix**, proportionate as the orchestrator asked - no bigger version, no scaling,
no alternating screens, not one pixel of the bitmap moved:

* `screeny_provision::wants_fixed_brightness(&Screen)` - true for exactly
  `Portal { layout: QrAndName }`, false for every other screen. Pure, host-tested
  (`crates/provision/tests/render.rs`, 2 new tests; the second re-asserts the golden
  hash and the independent decode, so "fix the brightness" can never quietly become
  "redraw the code").
* `firmware/src/provision.rs::fixed_brightness` supplies the number -
  `display::DEFAULT_BRIGHTNESS` - because which level is known-good belongs to the
  settings store, not to a `no_std` drawing crate. (`PanelScreen::as_screen` factored
  out of `render` so both go through one conversion.)
* `firmware/src/main.rs`: `SCREEN_BRIGHTNESS` / `SCREEN_BRIGHTNESS_NONE`, read by
  `target_oe_slots()` *below* the bench `OE_OVERRIDE` and through
  `clamp_brightness(_, BRIGHTNESS_CAP)`, so it can never exceed the cap.
* `firmware/src/net.rs`: the frame task writes it on the edge, in the same rank order
  it draws in (an update or a button screen covering the portal clears it), and bumps
  `BRIGHTNESS_DIRTY` so core 1 rewrites both framebuffers' OE windows. It is a
  presentation override, not a setting: nothing is written to flash, `BRIGHTNESS` is
  untouched, `GET_INFO` and `/api/v1/status` keep reporting what the owner set, and
  "restore it when the portal ends" is the edge clearing itself. No new field on
  `StatusReply`.

**What is still a hypothesis, and cannot be settled without the panel and the phone:**
that brightness is *the* reason it did not scan. What is established from the code is
only that the bitmap is byte-identical to the one decision 1 measured, that nothing
covers or alternates with it, and that the light was roughly 5/9 of what was measured.
Whether 9 slots is enough for that phone today, in that room, is a bench answer. The
owner's own view is that it may simply be marginal for a 64x32 panel - if the QR still
does not scan at the default brightness, then it is marginal by nature and this change
has cost nothing.

`FW_VERSION` -> **0.8.2** (0.8.1 skipped; it was spent as a bench upload image).

`tools/fw-size.sh` on the final default build: `.data` 60,108, `.bss` 110,704,
**`.stack` 25,792** (floor 24,576; 25,800 on 0.8.0 - eight bytes for the new atomic
and its alignment), `.rwtext` 67,740, image 1,028,189.
