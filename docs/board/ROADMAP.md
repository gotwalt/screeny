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
- [x] 101 art sends through `screeny::Link` (0dd4bae): `SenderOutput` behind the `sender` feature,
  `screeny-art play <piece> --to <name-or-addr>`, a "send to panel" switch in the Studio, and
  `budget.rs` replaced by a meter that runs the real encoder and the firmware's decoder (the preview
  is the decoded datagram). Accepted on the real panel, the owner's bar (2026-09-19):
  `clocks-numerals`, `overland` and `metaballs` at 30 fps, 0 drops, indexed frames 100% exact, device
  error counters all 0. Follow-ups: 110 (macOS LAN permission for the new binaries), 111
  (`Link` from a `Device`), 112 (is `sender` default-on)
- [x] 105 server-first Studio: axum + embedded UI, `/api/v1` + a newest-wins preview WebSocket,
  Tauri removed (292 -> 160 crates), two browsers stay in sync, a stalled one cannot slow the panel;
  streams to the real panel over the HTTP API and releases it on SIGTERM. Follow-ups 120, 121
- [x] 111 `Link::attach` (a resolved `Device`), 112 `sender` default-on in `screeny-art`
- [x] 066 the simulator dims by output-enable window, like the device (found: 25 real brightness
  steps, 1..=5 is black - card 136, needs the owner)
- [x] 080 `screeny-probe conformance`: 64 wire-level rules, same suite against sim and device;
  firmware 0.2.0: 60 passed, 0 failed, 4 skipped by design. Follow-ups 130-133
- 102 reconcile art's panel model and colour rules with the measured device - art session
- 104 piece runner / scheduler - **parked by the owner 2026-09-20** (no scheduling, no transitions: the focus is the pieces themselves)

Studio track (`docs/design/studio-vision.md`), in order: 101 Studio streams to the panel -> 105 server-first Studio, Tauri
removed -> 106 players/devices/state, built to be forgotten -> 107 docker-compose on
`workbench.local` -> ~~104 scheduler~~ (parked 2026-09-20), 102 panel-model reconcile (done), then **aesthetic work on the pieces**; demos ported into art when a piece is wanted from there.

Then, small and optional:
- 062 mDNS lifecycle (goodbye, re-announce, TTL)
- 080 wire conformance suite that can target sim and firmware (grow it from `screeny-probe`)
- 065 decoder throughput nit (`SOLID`); 082 sim squint view

## Device-web track (firmware session, cards 200-249) - started 2026-09-20

The owner un-deferred WiFi setup and widened it: an HTTP server on the device (status,
network settings, safe firmware update), a captive-portal soft-AP with a QR code on the
panel when there is no network, and the button as a WiFi reset. Decisions and the design
live in `docs/design/device-web.md`. A separate `firmware` Claude session coordinates it
and owns `firmware/`, the serial port and flashing.

- 200 research: partition table, settings store, OTA with rollback (in flight)
- 201 research: HTTP server, soft-AP, captive portal, the portal screen, RAM budget (in flight)
- 202 research: the button's GPIO from the stock image + a probe firmware (in flight)
- then the orchestrator writes the design and the build cards (210-249). 063 (persist
  settings) and 081 (sim WiFi states) come out of `parked/` as part of it.

## Deferred by the owner

- **WiFi setup**: no longer deferred - see the device-web track above. Until it lands,
  credentials are compiled in. It supersedes the serial console plan (old card 014).
- **Camera measurement** (old cards 012, 013, 061): the bench camera's colour
  accuracy is unknown; the owner will give visual feedback directly instead.

## Parked - `docs/board/parked/`, only on the owner's say-so

021 board revision detection, 032 real-content codec corpus, 040 DDP proxy,
041 control-channel auth, 060 upstream the brightness patch, 061 fixed-exposure
capture, 063 persist settings, 070 deeper fractal zoom, 081 sim WiFi states,
091 multi-device sender.
