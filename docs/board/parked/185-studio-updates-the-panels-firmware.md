---
id: 185
title: The Studio can update the panel's firmware
type: build
hardware: no
depends: [199]
owner:
branch:
---

## Goal

OTA works end to end on the panel since fw 0.7.0 (the firmware session, 2026-09-20). The
Studio is the thing that is always on and always talking to the panel: give `/panel` a way
to send it a firmware image and watch the update through to its verdict.

**Not started, and not to be started without the owner's word**: his focus is the
patches. Written down so the routes are not lost.

## Context (from the firmware session, verbatim in substance)

- `POST /api/v1/firmware[?activate=0]` on the PANEL (port 80): body is the raw
  `application/octet-stream` image made by `espflash save-image --chip esp32 --flash-size
  8mb --partition-table firmware/partitions.csv <elf> <out>.bin` (~1 MB, takes ~26 s; the
  device streams it into the inactive slot - one request with `Content-Length`, **never
  retried blindly**). The reply is always HTTP 200: `{"ok":bool,"written":u32,"error":
  null|"busy"|"bad_magic"|"wrong_chip"|"wrong_project"|"bad_checksum"|"bad_sha256"|
  "too_large"|"flash"|...,"activating":bool}`. `activate` absent/1/true/yes = activate
  (default); 0/false/no = stage only.
- With `activating: true` the panel reboots ~2 s after the reply, is back in ~30 s **on
  trial** (`status.fw_state: "pending_verify"`), confirms itself at >= 60 s if healthy
  (WiFi + address, a display swap, one HTTP request or 120 s) or reverts at 180 s; a reset
  during the trial makes the bootloader bring the old image back. **The Studio's ordinary
  10 s status poll counts as the HTTP request**, so a Studio that keeps polling and keeps
  streaming is what helps an update confirm - do not pause either during a trial.
- During the ~26 s upload the panel shows an "updating" screen and drops 10-20% of stream
  frames. Expected; keep streaming.
- The panel has **one connection worker**: the upload holds it for ~26 s. The status
  poller (one poller, one connection in flight - card 180) must stand aside for the
  upload and must not read the refused/timed-out polls during it as the panel being away
  (card 118's backoff, card 195's health flags).
- `screeny-probe fw-scan FILE` validates an image offline with the device's own checks
  (`crates/fwimage`): run the same validation in the Studio before POSTing, so a wrong
  file is refused on the page and not by the panel.
- The verdict is in `GET /api/v1/panic`'s `update` (card 199). Known rough edges on the
  firmware side (its card 246): after a confirmed update the device answers further
  uploads `busy` until rebooted; after a revert `status.fw_state` reads
  "invalid"/"aborted" (the other slot's state - the running image is fine).
- Where does the image come from? The Studio runs in a container on workbench and does
  not build firmware. Simplest honest answer: an upload control on `/panel` (the browser
  sends the file to the Studio, the Studio validates it and streams it to the panel).
  No credentials are involved; the panel's API is unauthenticated on the LAN, as is the
  Studio's.

## Acceptance

From `/panel`: choose an image, see it validated, send it, watch "updating -> on trial ->
confirmed" (or "reverted", with the reason) without the page ever claiming the panel is
lost, and without a second connection to the panel at any moment.

## Log

### Note (2026-09-21, from the firmware session's card 246)

`status.fw_slot` and `status.fw_state` now always describe ONE image - the running one
(spec 8.6 status bullet and 8.10 step 5 corrected; no JSON shape change). So after a
revert the page should no longer see "invalid"/"aborted" on a healthy panel; read the
spec before writing the trial/confirmed/reverted wording here and on card 199.
`crates/probe` and `crates/sim` gained an OTA model (off by default): use the simulator's
for this card's tests.

### Parked 2026-09-21 (owner: "I don't need to over-build this"; the three that matter are 188, 187, 199)

The firmware session already updates the panel over WiFi from the command line; this puts the same thing on a web page. Reopen when updating from the Mac is actually a nuisance. Card 199 (the update's outcome on /panel) goes ahead without it.
