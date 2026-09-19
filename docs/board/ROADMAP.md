# Roadmap

Phases and the cards planned for each. Cards get written into `backlog/` in full once
the phase before them has produced what they depend on.

## Phase 0 - bench (orchestrator)

- [x] Identify chip (ESP32-D0WD-V3, 8 MB flash, Gen 1 Tidbyt)
- [x] Install Xtensa Rust toolchain, espflash, esptool
- [x] Verified backup of stock flash in `backup/` (sha256 in backup/README.md)
- [x] Camera capture working (via `tools/cam-daemon.sh` in Terminal.app; see docs/research/000-bench-notes.md)

## Phase 1 - research (parallel)

- 001 firmware stack feasibility + compile-only skeleton
- 002 frame encoding lab
- 003 protocol / discovery / transport

## Phase 2 - design (orchestrator) - done

- [x] 004 `docs/design/protocol-v1.md` accepted, `docs/design/architecture.md`,
  host workspace skeleton

## Phase 3 - build

Wave 1 (parallel, no dependencies between them):
- 005 `crates/proto`: no_std wire types + the five decoders, hardened against bad input
- 007 `firmware/` display pipeline on hardware: orientation, ghosting, gamma,
  brightness without losing depth (020), status screen; stretch temporal dither (030)
- 010 `crates/demos`: fractal zoom + word clock renderers with a panel-model preview

Wave 2 (needs 005):
- 006 `crates/sim`: fake panel speaking the full protocol, window + headless
- 008 firmware networking: proto-based frame/control/telemetry/mDNS, source lock, idle (hardware)
- 009 `crates/screeny`: encoders + chooser, discovery, paced sender, CLI; wires in the demos
- 015 macOS code signing + Local Network permission for the `screeny` binary

Wave 3:
- 012 camera measurement harness: corner calibration, compare sent vs shown (hardware)
- 013 end-to-end fps / latency / loss and codec trade-offs on real hardware (hardware)
- 014 runtime WiFi provisioning (serial command + SET_WIFI)
- 030 device-side temporal dithering (if not done in 007)
- 031 sender encode-time budget; 032 real-content corpus for the codec chooser
- 021 board revision / colour order detection

## Parked - not scheduled, only on the owner's say-so

- 040 DDP proxy in the sender (LedFx/xLights -> native frames; host-side only, the
  firmware never speaks DDP). Owner is unsure it will be wanted.
- 041 control channel authentication (only matters on an untrusted LAN)
