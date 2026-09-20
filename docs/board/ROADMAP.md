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

## Phase 3 - build - done

Everything below is merged and verified on the real panel
(`docs/research/005-end-to-end.md`):

- 005 `crates/proto` - no_std wire types + five decoders, hardened against bad input
- 006 `crates/sim` - fake panel speaking the full protocol (found 17 spec ambiguities)
- 007 firmware display - gamma, depth-preserving brightness (020), temporal dither (030),
  status screen; ghosting turned out to be a camera artefact
- 008 firmware networking - protocol v1 on the device, display on core 1, `screeny-probe`
- 009 `crates/screeny` - encoders + chooser, discovery, paced sender, CLI (absorbed 031)
- 010 `crates/demos` - fractal zoom tour + word clock, wired in as `screeny fractal|clock`
- 015 macOS Developer ID signing + embedded Info.plist for Local Network permission
- 090 spec pacing fix

## Phase 4 - cleanup and hand-over (current)

The generative art system (`crates/art` + `crates/studio`) becomes the primary
sender. Owner's direction, 2026-09-19: cleanup first; WiFi setup and camera-based
measurement are deferred.

- [x] 011 sender embedding API: `Pixels`, `Sender::send`/`send_indexed` (exact), `Link`
  push API with auto-reconnect, Linux targets check clean, examples, brief section 5
- [x] 016 consolidation: `crates/receiver` (one receive core for sim + firmware),
  `crates/panel`, `crates/encode`, proto frame types everywhere, `spike/` deleted;
  device re-verified (conformance 22/22, lock-test 11/11, 60 s stream, 0 decode drops)
- [x] 017 one `crates/` workspace: the art system is `crates/art` + `crates/studio`
- [x] credentials out of git: firmware reads WiFi credentials at build time from
  outside the repo; history rewritten with same-length dummies and verified clean
  (every ref, message and object). Nothing is pushed until the owner says so
- [x] orchestrator: root README, CLAUDE.md, this roadmap, art brief refresh, stale
  worktrees/branches
- [x] 100 art system merged (42253d2): pipeline, Tauri studio, pieces. Verified: root 209
  tests, art 31 tests, and `screeny-art pipe clocks-numerals | screeny pipe --fps 60`
  into the simulator at 60 fps, 304 B/frame, `PAL4_LZ` exact, 0 drops
- 101 art sends through `crates/screeny` (after 011) - in progress on `card/101-art-sender-output`.
  Owner's acceptance (2026-09-19): it is done when it actually runs on the real panel, not only the sim.
- 102 reconcile art's panel model and colour rules with the measured device - art session
- 104 piece runner / scheduler (design with the owner) - art session

Studio track (`docs/design/studio-vision.md`), in order: 101 Studio streams to the panel -> 105 server-first Studio, Tauri
removed -> 106 players/devices/state, built to be forgotten -> 107 docker-compose on
`workbench.local` -> 104 scheduler, 102 panel-model reconcile, demos ported into art.

Then, small and optional:
- 062 mDNS lifecycle (goodbye, re-announce, TTL)
- 080 wire conformance suite that can target sim and firmware (grow it from `screeny-probe`)
- 065 decoder throughput nit (`SOLID`); 082 sim squint view

## Deferred by the owner

- **WiFi setup**: eventually a captive-portal flow with an HTTP settings UI on the
  device. Until then credentials are compiled in. Supersedes the serial/`SET_WIFI`
  plan (old card 014) and parks 063 (persist settings) and 081 (sim WiFi states).
- **Camera measurement** (old cards 012, 013, 061): the bench camera's colour
  accuracy is unknown; the owner will give visual feedback directly instead.

## Parked - `docs/board/parked/`, only on the owner's say-so

021 board revision detection, 032 real-content codec corpus, 040 DDP proxy,
041 control-channel auth, 060 upstream the brightness patch, 061 fixed-exposure
capture, 063 persist settings, 070 deeper fractal zoom, 081 sim WiFi states,
091 multi-device sender.
