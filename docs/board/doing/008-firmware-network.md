---
id: 008
title: firmware networking on protocol v1 (frames, control, telemetry, mDNS, lock/idle)
type: build
hardware: yes
depends: [005, 006, 007]
owner: claude-fable-5.1 (bench worker)
branch: card/008-firmware-network
---

## Goal

Replace the throwaway raw-RGB test receiver in `firmware/` with the real thing: the
device speaks `docs/design/protocol-v1.md` completely, using `crates/proto` for every
byte of wire logic, and holds 30 fps with all five codecs on real WiFi while the panel
keeps refreshing cleanly. After this card a spec-conforming sender can drive the panel.

## Context

- Spec: `docs/design/protocol-v1.md` (now includes the 17 clarifications the simulator
  found - section 11 items 14-30). Architecture: `docs/design/architecture.md`.
- `crates/proto` (README has the API): `Packet::parse`, `peek`, `dec::decode(codec,
  payload, dst)`, `control::{Request, Reply, Telemetry}`, `txt::DeviceInfo`. It builds
  for `xtensa-esp32-none-elf` with `-Zbuild-std=core` (already how firmware builds).
  Depend on it by path. `decode_pal8_lz` needs ~768 B of stack: size the task stack.
- **Reference implementation:** `crates/sim/src/core.rs` is the receive state machine
  (seq, newest-wins, source lock, idle, counters, control ops, rate limits) written as
  a pure core with no I/O and no clock. Port its logic; do not reinvent it. If it can be
  made `no_std` and shared cheaply, even better - but do not destabilise the sim to get
  there; copying with attribution is acceptable.
- Card 007's findings that shape this card (read its log in `docs/board/done/`):
  1. **The display task holds the frame lock across a synchronous ~3 ms render, on the
     same single-threaded executor as the frame task**, so "drain the socket, newest
     wins" can never actually run concurrently, and temporal dithering rewrites the
     framebuffer every refresh, costing ~40% of a core permanently. The ESP32 has two
     cores: **move the display/dither work to core 1** (esp-rtos second-core executor)
     and keep WiFi, net, decode and control on core 0. Hand frames across with a
     lock-free or very short critical-section swap of the decoded RGB888 buffer. Measure
     refresh stability and decode latency before/after.
  2. There is a ~640 ms stall once per boot during WiFi association. Find out whether
     it can recur on reconnect; it must not freeze the panel (core split should fix).
  3. Ghosting was a camera artefact; brightness is OE-duty (25 real steps, depth
     intact); temporal dithering works. Keep all of that.
- Bench: device is `192.168.7.221` / `screeny.local`, this Mac is wired to the same
  LAN. The simulator (`cargo run -p screeny-sim`) is the known-good receiver to compare
  against. A sender crate (`crates/screeny`, card 009) may land on main while you work;
  do not wait for it.

### Bench rules (you have hardware access; these are not optional)

Same as card 007 - read `docs/board/done/007-firmware-display.md` "Bench rules" and
`docs/research/000-bench-notes.md`. In short: bench tools by ABSOLUTE path in the main
checkout (`/Users/aaron/src/screeny/tools/fw-run.sh <abs elf> NAME [secs]`,
`/Users/aaron/src/screeny/tools/cam-request.sh NAME [clip N]`), capture names prefixed
`c008-`, baud <= 230400, one serial process at a time, never erase flash, never touch
`backup/`, brightness stays capped, no flashing above 3 Hz, camera daemon problems ->
stop and report. A still cannot photograph the dithered panel honestly below ~sRGB 40;
use a clip and ffmpeg `tmix`.

## Deliverables

1. `firmware/` tasks per architecture.md: `frames` (UDP 49374: drain, validate with
   proto, newest valid frame for the active source, decode into a back buffer that is
   never partially visible, hand to display), `control` (UDP 49375: every op in the
   spec; `SET_WIFI` may return `ERR_UNSUPPORTED` until card 014; `REBOOT` works),
   source lock + idle state machine (hold, then status screen; never blank to black),
   telemetry with every counter the spec defines, piggybacked `STATS_REQ` replies,
   `BUSY` replies rate-limited, mDNS TXT built from the same `DeviceInfo` bytes as
   `GET_INFO`, runtime brightness via the control op (persisting it is optional).
2. Display on core 1 as described above, with before/after measurements.
3. `screeny-probe`: a small host tool for bench testing, as a second binary in
   `crates/sim` (it already has the in-process sender test helpers) or a tiny new crate:
   `--addr HOST[:PORT]`, subcommands `info`, `ping N`, `stats`, `brightness N`,
   `stream --codec <id|all> --fps 30 --secs N` (streams pre-encoded payloads from
   `crates/proto/tests/vectors/` plus a generated moving `SOLID`/`PAL4_LZ` pattern, with
   `STATS_REQ` every 30 frames, prints sent vs device telemetry), `lock-test` (second
   source gets `BUSY`, takeover after timeout, `FINAL` releases). Prove it against the
   simulator first, then the device.
4. Evidence on real hardware, in the card log: `info` output; `ping` RTT distribution
   (n >= 200); for each codec a 60 s 30 fps run with sent / rx / shown / dropped-by-cause
   / decode_us / inter-arrival jitter from telemetry; a 10-minute soak on the heaviest
   codec with heap and refresh rate at start and end; lock-test passing; idle fallback
   captured on camera; a camera clip of a moving pattern showing no tearing or flicker
   while streaming; behaviour across a WiFi drop if you can provoke one safely (e.g.
   `REBOOT` mid-stream and watch the sender-side recovery).
5. Update `docs/design/architecture.md` (firmware section) to match what you built.

## Acceptance

Every codec streams at 30 fps for 60 s with zero decode drops and < 1% total loss on
the bench network; the 10-minute soak ends with stable heap and refresh; control ops
and the lock state machine behave as the spec says on the real device; the device is
left running this firmware showing its status screen.

## Log
