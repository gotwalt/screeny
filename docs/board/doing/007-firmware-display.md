---
id: 007
title: firmware/ - display pipeline on real hardware (gamma, ghosting, brightness, status text)
type: build
hardware: yes
depends: [001, 004]
owner: claude-fable-5.1 (bench worker)
branch: card/007-firmware-display
---

## Goal

Turn the bring-up spike into the real `firmware/` project and make the *display half*
right on the actual panel: correct gamma, no ghosting, brightness control that does
not cost bit depth, known orientation, and a status screen. Networking stays as the
spike has it (raw RGB888 test receiver); card 008 replaces it once `crates/proto`
exists. You are the only worker with hardware access while this card is in `doing/`.

## Context

- Read first: `docs/research/004-first-bringup.md` (what works, what does not),
  `docs/research/001-firmware-stack.md`, `docs/design/architecture.md` (firmware
  section), `docs/research/000-bench-notes.md` (bench quirks), cards 020, 021, 030.
- Start by copying `spike/fw-skeleton` to `firmware/` (package `screeny-fw`). Leave the
  spike frozen. Keep the pinned dependency set; the version window is one release wide.
- This unit's colour lines are rotated relative to the hdk pin names; the spike
  already has the fix. WiFi SSID is `Example-Wifi1` (capital T).
- Build: `. ~/export-esp.sh && cd firmware && cargo build --release`.

### Bench rules (you have hardware access; these are not optional)

- **Always call the bench tools by their absolute path in the main checkout**,
  `/Users/aaron/src/screeny/tools/...`, not the copies in your worktree. The camera
  daemon watches the main checkout's `captures/req/`, so captures and logs land in
  `/Users/aaron/src/screeny/captures/`. Pass `fw-run.sh` an absolute path to your ELF.

- Flash + serial log + camera still in one step:
  `tools/fw-run.sh firmware/target/xtensa-esp32-none-elf/release/screeny-fw NAME [secs]`
  -> `captures/NAME.log`, `captures/NAME.jpg`. It refuses to flash without the stock
  backup. `espflash monitor` resets the chip when it attaches, so logs start at boot.
- Camera only: `tools/cam-request.sh NAME` (still) or `tools/cam-request.sh NAME clip 3`
  (mp4). The capture daemon runs in the owner's Terminal.app; if `cam-request` says it
  is not running, stop and report - do not try to start ffmpeg yourself (the Claude
  app has no camera permission and it just hangs).
- Look at your captures with the Read tool. The panel occupies roughly x 405-1560,
  y 210-805 of the 1920x1080 frame; crop with ffmpeg before reading to save effort.
  The camera auto-exposes and clips on bright LEDs: dim the panel for colour judgements.
- Serial baud must never exceed 230400. One process on the serial port at a time:
  make sure no `espflash` is left running (`pgrep -x espflash`) before flashing.
- To send test frames to the running device, the spike's receiver takes a raw RGB888
  datagram on UDP 49374 (first 490 pixels). The device gets 192.168.7.221 by DHCP
  (check the log) and answers mDNS as `screeny.local`. Python one-liners are fine.
- Keep the brightness cap. The panel is powered from laptop USB. Never display
  full-white full-brightness frames; no flashing patterns above 3 Hz.
- `captures/` is git-ignored. Copy the few images that matter into
  `docs/research/img/` (downscaled JPEG, < 200 KB each).

## Deliverables

1. `firmware/` builds clean, boots, joins WiFi, shows a status screen.
2. **Orientation proven** with an asymmetric pattern (e.g. a corner marker + an "F"
   glyph): origin top-left, x right, y down, no mirroring. Fix if wrong.
3. **Ghosting fixed** (the faint copy of lit rows 1-2 rows below): esp-hub75
   `trail-blank-N` / `inter-row-blank-N` or whatever it takes. Before/after captures.
4. **Gamma**: a 256-entry sRGB8 -> panel-level LUT applied in the display path, so an
   sRGB grey ramp looks perceptually even on camera and mid-grey is not blown out.
5. **Brightness without losing depth** (card 020): investigate OE-duty / blanking-time
   control in esp-hub75 (patch or fork if needed; vendoring under `firmware/vendor/` is
   acceptable) so brightness is a runtime 0-255 value that leaves 6 bit planes intact.
   If that is genuinely not achievable, document why and implement the best fallback.
   Default to a comfortable indoor level that does not clip the bench camera.
6. **Status screen**: tiny bitmap font (5x7 or 4x6), shows "screeny", WiFi state and
   the IP address once known; drawn only when no stream is active.
7. Measure and log: refresh rate, time to convert RGB888 -> DMA buffer (the per-frame
   display cost), free heap. Put a frame-time budget table in your log.
8. Stretch, only if everything above is done: **temporal dithering** (card 030) -
   carry the sub-LSB remainder across refreshes. If you take it on, move card 030 too.
   If not, leave 030 in the backlog with anything you learned.

Fold the outcomes of cards 020 and 021 into their files (move them to review too if
completed; otherwise append what you learned and leave them in backlog).

## Acceptance

Camera captures in the card log show: correct orientation, no visible ghosting, an
even grey ramp, the status screen with a readable IP, and the same image at two
brightness levels with all ramp steps still distinguishable at the lower one. The
device is left running this firmware.

## Log
