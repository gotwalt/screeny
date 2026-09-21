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
