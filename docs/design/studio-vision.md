# Screeny Studio: from design tool to the thing that runs the panels

Status: **draft for the owner's review** (2026-09-19). Owner's vision, orchestrator's
plan. Open questions are at the end.

## Vision (owner)

Screeny Studio is run two ways from one codebase:

1. **Locally**, for experimenting: design pieces, preview them as LEDs, push them to a
   panel on the desk.
2. **As a dockerized web app** on the network: opened in a browser from anywhere on
   the LAN, it controls what is streaming to the device(s) - which piece, which
   parameters, which panel, on what schedule - and keeps streaming whether or not a
   browser is open.

First milestone: the Studio streams to the real hardware. Then keep going.

## What exists today

- `art/screeny-art`: pieces -> limiter -> panel-aware quantise -> `WireFrame` ->
  `Output` trait. Headless binary (`list`, `pipe`, `snapshot`). Optional wgpu.
- `art/studio`: Tauri v2 desktop app, ~1.2k lines. One `Engine` on its own thread; 11
  Tauri commands (`bootstrap`, `frame`, `set_piece`, `set_param`, `reset_params`,
  `set_seed`, `set_settings`, `set_playback`, `piece_playing`, `piece_act`,
  `restart`); a static three-file front end whose every call goes through one
  `invoke()` wrapper that already has a non-Tauri (mock) path. The window polls
  `frame` for the newest frame.
- `crates/screeny`: sender library (card 011 is adding exact indexed sends, a push
  API and auto-reconnect), discovery, control client (brightness, identify, stats).
- The firmware arbitrates between senders itself (source lock, `BUSY`, idle fallback).

## Architecture

**Server-first.** The Studio becomes one Rust binary, `screeny-studio`, that is an
HTTP + WebSocket server (axum) embedding the static UI. Everything the UI does is an
HTTP/WS call. The two ways of running it are then the same program:

- *local*: `screeny-studio` on localhost, browser tab (or an optional thin Tauri shell
  whose webview points at the local server - a launcher, not a second interface);
- *docker*: the same binary in a container, bound to the LAN.

This is cheap because of how the Studio was built: the 11 commands become 11 routes,
the UI's single `invoke()` wrapper becomes `fetch()`, and frame polling becomes a
WebSocket that pushes preview frames (6 KB RGB at 30-60 fps is ~200-370 KB/s).

```
                       ┌──────────────── screeny-studio (one process) ────────────────┐
 browser(s) ──HTTP/WS──┤ api ── Players: one per device ── piece + params + seed + fps │
                       │          │  render (screeny-art pipeline)                    │
                       │          ├─> preview hub ──WS──> browsers                    │
                       │          └─> screeny::Sender (exact indexed, reconnects) ────┼──UDP──> panel(s)
                       │ device registry: mDNS browse + manual addresses; control ops │
                       │ state store: what plays where, playlists/schedule (a volume) │
                       └───────────────────────────────────────────────────────────────┘
```

Properties that matter:

- **Streaming does not depend on a browser.** Players belong to the server. The UI is
  a remote control and a preview. Restart the container and it resumes what it was
  playing (state store).
- **One player per device.** Same piece to several panels, or different ones. The
  multi-device sender (parked card 091) and the piece runner/scheduler (card 104)
  land here, as parts of the server rather than as separate programs.
- **Device controls** (brightness, identify, name, stats, reboot) are proxied through
  the existing control client. The device's own future captive-portal/HTTP settings
  page is a separate thing, for WiFi setup only.
- **Editing vs playing.** The design view (sliders, seeds, pause, scrub) drives a
  *preview* player that can be pointed at a device ("send this to the desk panel") or
  not. Promoting a tuned piece to "what plays on panel X" is an explicit action.

## Docker: the two things that bite

1. **mDNS and LAN UDP.** On **Linux with `--network host`** the container sees the
   LAN: discovery works, unicast UDP works. On **macOS (Docker Desktop / Colima) the
   container is inside a VM**: no multicast, so no discovery; outbound unicast UDP
   through NAT does work, including telemetry replies to the same socket. So the
   server must always accept manually configured device addresses, and discovery is a
   convenience on top. The real deployment target is a Linux box.
2. **GPU.** wgpu pieces need Vulkan in the container: NVIDIA container toolkit on a
   GPU box, or Mesa lavapipe (CPU Vulkan) as a slow fallback; `--no-default-features`
   drops GPU pieces entirely. There is no GPU passthrough in Docker on macOS at all.
   CPU pieces (the clocks, plasma) are unaffected. The server should report which
   pieces are available in its environment rather than fail.

Also: the web UI has no authentication today and neither does the device control
channel (parked card 041). Fine on a home LAN; do not expose the port to the
internet without a reverse proxy or a tailnet in front.

## Repository shape

One cargo workspace under `crates/` (owner's direction), firmware outside it because
it has a different target and toolchain:

```
crates/
  proto/      no_std wire format + decoders            (firmware + host)
  receiver/   no_std receive state machine (card 016)  (firmware + sim)
  screeny/    sender library + `screeny` CLI
  sim/        fake panel          probe/   bench instrument
  art/        was art/screeny-art: pieces, pipeline, headless bin   (pkg screeny-art)
  studio/     was art/studio: the server + embedded UI              (pkg screeny-studio)
  demos/      fractal + word clock - to be ported into art as pieces, then retired
firmware/  lab/  docs/  tools/
```

`default-members` excludes `studio` (and any Tauri shell) so that `cargo test` at the
root stays fast and does not need GUI system libraries; heavy crates build with `-p`.
One `Cargo.lock`, one `target/` (the separate target dirs currently cost gigabytes).

## Sequence

1. **Land what is in flight**: 011 (sender embedding API) and 016 (consolidation).
   Both touch the crates that would move; reorganising under them causes conflicts.
2. **017 reorg** (orchestrator, mechanical, ~an hour): `git mv` into `crates/`, one
   workspace, `default-members`, fix paths/docs. Done before the art session starts
   new work, so nobody builds on the old layout.
3. **101 Studio streams to hardware**: `SenderOutput` on `crates/screeny`; the engine
   gets a device target (picker: discovered + manual); real encoder stats replace the
   estimates. Milestone: design a piece in the Studio, watch it on the panel.
4. **105 server-first Studio**: axum server + embedded UI, routes replacing Tauri
   IPC, WS preview; Tauri kept only as an optional shell (or dropped - open question).
5. **106 players and devices**: device registry, one player per device, state store,
   resume on restart; device controls in the UI.
6. **107 Dockerfile + compose**: Linux host-network profile, macOS/NAT profile with
   manual addresses, CPU-only and GPU variants.
7. **104 runner/scheduler**, 102 (panel model reconcile), porting the demos into art.

## Open questions for the owner

1. **Where will the container actually run?** A Linux box (then host networking +
   discovery + optional GPU all work), or Docker/Colima on the Mac (then manual
   addresses, CPU pieces only)? It decides which profile is the real one.
2. **Keep a Tauri desktop shell at all?** Server + browser tab is simpler and one
   fewer heavy dependency; a shell gives a dock icon and a window. Either works with
   the server-first design.
3. **Will more panels exist soon?** It sets how early multi-device (106) matters.
