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

### Step 1 - the server exists and Tauri is gone

`crates/studio` is now a library plus a binary. The engine is unchanged in substance -
it is still one piece rendered off the wall clock on its own OS thread, still the
`screeny-art` pipeline, still card 101's `SenderOutput` - and everything that used to
be a Tauri command is a route.

| file | what it is |
|---|---|
| `src/engine.rs` | `Engine` moved out of `main.rs` whole: `tick`, the 52-byte frame packet header, `snapshot`, and one method per command (`set_piece`, `set_param`, ... `set_panel`, `panel_status`). Plus `close_panel`, which is new: stopping the server sends `FINAL` instead of leaving the panel to time out. |
| `src/api.rs` | the thirteen routes, the `StateEvent`/`StatusEvent` shapes, and `ApiError` (a 400 with `{"error": ...}`) |
| `src/ws.rs` | `GET /api/v1/ws`: frames, state changes, heartbeat |
| `src/ui.rs` | the three UI files, `include_bytes!`d, or read from `--ui-dir` |
| `src/lib.rs` | `Config`, `AppState`, `Studio::bind/serve/spawn`, the engine thread, the status task |
| `src/main.rs` | argument parsing (`--listen`, `--ui-dir`, `--help`) and startup; 3 unit tests |

Deleted: `build.rs`, `tauri.conf.json`, `capabilities/default.json`, `icons/icon.png`,
the generated `gen/` directory and the `.gitignore` line that hid it.

The API, in full. Names are the Tauri command names unchanged, so the UI's calls moved
without being rewritten. Reads are `GET`, changes are `POST` with a JSON body:

```
GET  /api/v1/bootstrap      -> {pieces, payload_bytes, state}
GET  /api/v1/frame          -> application/octet-stream, 52 + 64*32*3 = 6196 bytes
GET  /api/v1/piece_playing  -> Playing | null
GET  /api/v1/panel_status   -> PanelStatus | null
POST /api/v1/set_piece      {id}                    -> StudioState
POST /api/v1/set_param      {id, value}             -> StudioState
POST /api/v1/reset_params   {}                      -> StudioState
POST /api/v1/set_seed       {seed?}                 -> StudioState   (null = a new one)
POST /api/v1/set_settings   {settings}              -> StudioState
POST /api/v1/set_playback   {paused, speed, fps}    -> StudioState
POST /api/v1/piece_act      {action}                -> Playing | null
POST /api/v1/restart        {}                      -> StudioState
POST /api/v1/set_panel      {on, to}                -> PanelStatus | null
GET  /api/v1/ws             -> websocket
```

`PanelStatus` and `Playing` cross the boundary exactly as card 101 left them: both are
`Serialize` in `crates/art`, and nothing in `crates/art` had to change for this card.

Three kinds of message come out of the WebSocket, and all three are newest-wins:

- **binary**: one frame packet, the same layout the desktop app handed over;
- `{"type":"state","rev":N,"from":"<client>"|null,"state":{...}}` whenever anything
  changes;
- `{"type":"status","playing":...,"panel":...}` twice a second.

A browser sends its own id in `X-Studio-Client` on every change and as `?client=` on
the socket, and the server does not echo a browser its own change - otherwise adopting
the state would fight with the slider still under the mouse. A resync (`from: null`)
is adopted by everyone.

**Bounded by construction**, which the card asks for by name and the deployment means
literally:

- frames: a `tokio::sync::watch` cell. One slot, overwritten in place. The engine
  thread calls `send_replace`, which never blocks and never allocates a queue, so a
  browser that has stopped reading cannot hold the engine or the panel link back. It
  simply misses frames.
- state changes: a `broadcast` channel of **32**. A socket that falls that far behind
  gets `Lagged`, and is sent the current state instead of the 32 it missed - which is
  the right answer anyway, since the newest state is the state.
- status: another one-slot `watch`, filled by a single task, so one poll of the link
  serves however many browsers are open.
- a send that cannot complete in 3 s closes that socket (`ws::STALL`).

The UI kept its look and its behaviour; what changed is underneath it. `invoke(cmd,
args)` is now `fetch('/api/v1/' + cmd)`, the frame poll is gone (frames arrive on the
socket and `requestAnimationFrame` draws the newest one that has arrived), and the two
`setInterval` polls - `piece_playing` every 500 ms, `panel_status` every second - are
gone with it: that is what the status heartbeat is. The socket reconnects with
backoff, so restarting the server no longer means reloading the tab. "Send to panel"
is untouched apart from its transport, including the status line.

Smoke test (`--listen 127.0.0.1:18787`, under `timeout`, nothing left running):
`/` 200 (7658 B), `/main.js` 200 (19701 B), `/../Cargo.toml` **404**, `/api/v1/frame`
200 with exactly 6196 bytes, `set_piece plasma` 200 with the new state,
`set_piece nope` **400** `{"error":"no piece called `nope`"}`.

### Step 2 - the tests, and what they measured

`crates/studio/tests/`, 11 tests, no network, no device, every server on an ephemeral
loopback port and stopped when the test ends (`Running` is a guard: dropping it stops
the engine thread, sends `FINAL` and stops the server).

`tests/common/mod.rs` is a browser in about 200 lines: enough HTTP/1.1 to call the API
(one connection per request, `Connection: close`, so there is no framing to get wrong)
and enough of RFC 6455 to read what the server pushes. Hand-written rather than
borrowing a client crate - or the server's own library, which would hide exactly the
bug worth catching.

`tests/api.rs` (8):

| test | what it pins |
|---|---|
| `the_api_round_trips` | all thirteen routes: every read, every change, the state each change returns, and the two 400s (`no piece called ...`, `plasma has no parameter ...`). Speed clamps at 8x; an fps that is not offered is ignored; `set_seed {"seed":null}` picks a new one; `restart` keeps the seed |
| `the_engine_runs_with_nobody_watching` | the frame's sequence number advances with no browser connected |
| `the_ui_is_served_from_the_binary` | `/`, `/main.js`, `/style.css` are 200; `/nope.js`, `/../Cargo.toml` and `/sub/dir.js` are 404 |
| `the_socket_delivers_frames` | the hello state, then frames, sequence numbers advancing, > 30 in a second |
| `two_browsers_see_each_others_changes` | Alice's change reaches Bob tagged `from: "alice"`, Bob's reaches Alice - and Alice is **not** sent her own, checked by waiting 600 ms for the server to be wrong |
| `a_late_browser_starts_in_step` | a browser connecting after two changes is handed the current state, `from: null` |
| `the_socket_carries_the_heartbeat` | `{"type":"status"}` carries "now playing" and the panel link: the two polling timers the UI used to run |
| `a_frame_packet_is_a_header_and_a_picture` | 52 + 64*32*3 = 6196 bytes, colours and encoded size inside their real bounds |

`tests/panel.rs` (3), against `screeny-sim` on loopback - the same receiver card 101's
acceptance used, on a consecutive port pair in 50700..50780, mDNS off:

- **`send_to_panel_streams_the_preview_to_the_device`**. `POST /api/v1/set_panel
  {"on":true,"to":"127.0.0.1:<port>"}` and the studio streams; `GET panel_status` is
  the UI's status line. With the piece paused and the limiter off, every frame is the
  same frame, so "the device shows what the browser draws" is a statement about bytes:
  the device's decoded frame **equals** the preview out of the WebSocket, and the ten
  frames it showed are identical to each other. Measured:
  `20 offered, 10 sent, 9 coalesced, 1 dropped before the link was up; codec 0x10
  (pal8-lz), 1379 B/frame, exact 10, fallback 0`. Turning the switch off returns
  `null` and drops the link, which sends `FINAL`.
  The one dropped frame is the engine offering a frame while the deferred link was
  still finding the device - by design, and the test pins that nothing is dropped
  after the link is up.
- **`a_stalled_browser_does_not_hold_up_the_engine_or_the_panel`**. A browser whose
  receive buffer is 2 KB - smaller than one 6196-byte frame - connects and stops
  reading. With it wedged and still connected, over two seconds: **engine 120 ticks,
  panel link 60 frames, a healthy browser 120 frames**. That is 60 fps and 30 fps on
  the nose, which is what the numbers would be with no stalled browser at all.
- **`a_stalled_browser_is_eventually_dropped`**. The same wedged socket is closed by
  the server after `ws::STALL` (3 s), rather than being kept for ever: `studio: a
  browser stopped reading for 3s; closing its preview socket`.

`socket2` is a dev-dependency for that 2 KB receive buffer - it is how the stall is
made to happen in a second rather than after a megabyte of kernel buffer fills - and
`screeny-proto` for the type in the simulator's frame sink.
