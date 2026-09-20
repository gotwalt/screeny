---
id: 105
title: Server-first Studio - axum + embedded UI, Tauri removed
type: build
hardware: no
depends: [017, 101]
owner: worker-105
branch: card/105-studio-server-first
---

## Goal

`screeny-studio` becomes one ordinary Rust binary: an HTTP + WebSocket server that
embeds the static UI. Same program locally (`localhost`) and in Docker. No desktop
window. Design: `docs/design/studio-vision.md`.

## Context

- Today: Tauri v2 app, ~1.2k lines; one `Engine` on its own thread; 11 commands
  (`bootstrap`, `frame`, `set_piece`, `set_param`, `reset_params`, `set_seed`,
  `set_settings`, `set_playback`, `piece_playing`, `piece_act`, `restart`); the UI's
  calls all go through one `invoke()` wrapper with a non-Tauri fallback.
- Owner's decision: drop the desktop window entirely.

## Deliverables

- axum server; UI files embedded at build time (`include_dir`/`rust-embed`), with a
  `--ui-dir` override for live editing. `--listen` (default `127.0.0.1:8787` locally;
  compose sets `0.0.0.0:8787`).
- JSON API replacing the 11 commands 1:1 first (keep names), versioned under `/api/v1`.
  Preview frames over a WebSocket (binary, newest-wins, drop if the client is slow -
  a slow browser must never back-pressure a player).
- UI: `invoke()` -> `fetch()`/WS. Works from a second browser at the same time
  (state changes broadcast over the WS so two tabs stay in sync).
- Tauri, its config, icons, capabilities and build script removed; dependency tree
  and build time before/after in the log.
- Tests: API round-trips against an in-process server; WS delivers frames and
  survives a stalled client; two clients see each other's changes.
- Bounded by construction: no unbounded channels or buffers anywhere.

## Acceptance

`cargo run -p screeny-studio` and a browser tab give everything the Tauri app did,
including streaming to a device (card 101), with no GUI system libraries needed to
build.

## Log
