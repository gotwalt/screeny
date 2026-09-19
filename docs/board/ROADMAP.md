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

## Phase 2 - design (orchestrator)

- 004 finalise `docs/design/protocol-v1.md` and `docs/design/architecture.md`
- workspace layout: `crates/proto` (no_std codec + packet types, shared),
  `firmware/`, `crates/screeny` (sender lib + CLI), `crates/sim`

## Phase 3 - build (parallel where possible)

- 005 `proto` crate: packet types + codecs, no_std, property-tested against the lab
- 006 host simulator: receives the protocol, renders the panel in a window/terminal
- 007 firmware bring-up on hardware: panel test pattern, brightness cap (hardware)
- 008 firmware networking: WiFi, UDP receive, decode, display, telemetry, mDNS (hardware)
- 009 sender library + CLI: discovery, pacing, stats, test patterns
- 010 fractal zoom sender
- 011 word clock sender: spells out the current time, animated transitions
- 012 camera measurement harness: capture, locate panel, compare with sent frames (hardware)
- 040 DDP proxy in the sender: LedFx/xLights -> native frames (host-side only; firmware never speaks DDP)

## Phase 4 - tune

- 013 end-to-end fps / latency / loss measurement and codec trade-offs on real hardware
- 014 runtime WiFi provisioning
