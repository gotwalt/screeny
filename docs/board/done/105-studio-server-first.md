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

### Step 3 - an ordinary workspace member, and what Tauri cost

`crates/studio` no longer needs GUI system libraries, so the root manifest's
`default-members` list is **gone**: `members = ["crates/*"]` is now the whole default
build, and a plain `cargo build` / `cargo test` covers all ten host crates. The
sentence in `CLAUDE.md` that said otherwise is updated, and so is the studio's row in
the root `README.md`.

One consequence worth knowing, because it lands in card 112's lap: with the studio in
the default build, cargo unifies features across the workspace, so a plain `cargo
test` now compiles `screeny-art` **with `sender`** - which means `crates/art/tests/
sender.rs` runs by default instead of reporting "0 tests", and `target/*/screeny-art`
keeps its `play` subcommand instead of losing it to the next plain build. That is the
trap the orchestrator recorded at the end of card 101, closed as a side effect. It
does not decide card 112: `cargo test -p screeny-art` on its own still has `sender`
off.

**What Tauri cost, measured.** Cold builds of `cargo build -p screeny-studio` into an
empty target directory on this (shared, and busy) bench, `--timings` both times:

| | before (Tauri v2) | after (axum) | |
|---|---|---|---|
| crates in the dependency tree | **292** | **160** | `cargo tree -e normal,build`, deduplicated |
| compilation units | 434 | 209 | build scripts and proc-macro builds included |
| CPU-seconds of compilation | **296.7** | **189.3** | sum of every unit, from the `--timings` report |
| wall clock, cold, 12 cores | 46.2 s | 34.8 s | noisy: three other workers are building on this machine |
| direct dependencies | `tauri`, `tauri-build`, `screeny-art`, `serde`, `serde_json` | `axum`, `tokio`, `screeny-art`, `serde`, `serde_json` | |
| slowest units | `tauri-utils` 12 s, `objc2-app-kit` 9 s | `tokio` 10 s, `wgpu-core` 8 s | |

**No GUI system libraries.** `webkit2gtk`, `gtk`, `tao`, `wry`, `winit` and
`objc2-app-kit` are all gone from the tree. What is left of the platform crates is
`objc2-metal`/`objc2-quartz-core` under **wgpu**, which is `screeny-art`'s GPU pieces
(Vulkan on the Linux box, per the vision document) and not a window toolkit - and
`objc2` under `ctrlc`. To make that checkable rather than a claim, the studio now has
a `gpu` feature, default on, that forwards to `screeny-art/gpu`:

```
cargo tree -p screeny-studio                        160 crates
cargo tree -p screeny-studio --no-default-features   120 crates, no graphics driver at all
cargo build -p screeny-studio --no-default-features  clean, 16 s
```

so a box with no usable adapter (or a container image that would rather not carry
Mesa) has a build that cannot want one. The CPU pieces are all still there.

`cargo test --release --no-fail-fast` at the workspace root: **302 passed, 0 failed**
(283 before this card, plus the studio's 11 and 8 that the feature unification above
now runs). `crates/screeny`'s timing-sensitive pacing tests passed first time; card
093's flake did not appear.

### Step 4 - stopping cleanly, which is what releases the panel

Found while thinking about what card 107 will do to this process: `docker stop` sends
`SIGTERM`, and `Drop` does not run on a signal - the same edge card 101 hit in
`screeny-art play`, where it needed a ctrl-c handler to send `FINAL`. Without one here
the panel would be left holding the last frame until its stream timeout on every
container restart.

So `Studio::stop_on_signal()` (Ctrl-C or `SIGTERM`) sets the stop flag; graceful
shutdown then unwinds the whole process in order. Two things had to change for that
to be true rather than hopeful:

- every open preview socket watches the stop flag as well. Otherwise axum's graceful
  shutdown waits for connections to end, and a browser that is perfectly happy never
  ends one. New test: `a_connected_browser_does_not_hold_the_shutdown_open`.
- `close_panel()` on the way out of both the engine thread and `serve()`.

Measured, with a `screeny-sim` on loopback:

```
POST /api/v1/set_panel {"on":true,"to":"127.0.0.1:50790"}
  state up, connected, 30.0 fps, 91 sent / 90 coalesced, indexed exact 91, pal8-lz, 400 B
  sim: 29.6-30.4 fps LIVE, rx 128 shown 128, gaps 0 stale 0 super 0 dec 0 rej 0
kill -TERM <studio>
  studio: stopping; releasing the panel        <- and the process exits
  sim:  lock released by 127.0.0.1:50177: Final
        state Live -> Hold
```

The engine ran at 60 and the link put 30 on the wire, with the device superseding
nothing - the same result card 101 measured, now driven by an HTTP request.

**A bug this shook out**, worth recording because it would have been intermittent and
maddening: `reset_params` and `restart` take no arguments, so their handlers had no
body extractor - but the UI `POST`s `{}` to them, and a server that closes a
connection with a request body still unread gets a **TCP reset** rather than a clean
close. The test client saw `ECONNRESET` once in about ten runs; a browser would have
seen a failed `fetch` just as rarely. Both handlers now read and discard the body.
Three consecutive full runs of the studio's 15 tests after the fix: green, green,
green.

### Step 5 - how far the front end was actually checked, and two cards

The Claude browser extension was **not connected** in this worker's environment, so
the page was never rendered. What was checked instead:

- `node --check main.js` (a one-off syntax check from the shell - the crate still has
  no Node toolchain and no build step);
- every `#id` `main.js` reaches for exists in `index.html`: **none missing**. Every
  command it calls is a route the server has: **none missing**. Three routes are no
  longer called by the UI - `frame`, `piece_playing`, `panel_status` - which is
  intentional: frames and status come over the socket now, and the routes stay for
  parity, for tests, and for anything that would rather poll;
- the server's tests pin every message shape the page consumes.

That covers wiring, not rendering. `docs/board/backlog/121-studio-front-end-check.md`
is the card for closing it; the orchestrator opening the tab for the panel run is the
immediate answer.

One fix came out of the review pass: `adopt()` now refreshes the bound controls as
well as rebuilding the parameter sliders, so a piece change made in *another* browser
updates this one's playback and panel-model controls too, not only its sliders.

**Cards written, not done (reserved range 120-124):**

- **120 - Preview bandwidth.** Every socket gets every frame: 6196 B at 60 fps =
  372 KB/s per open tab, visible or not. Safe (one slot, newest wins) but wasteful,
  and it will be the studio's idle cost in `docker stats` once card 107 lands. Also
  notes the slider-drag broadcast rate.
- **121 - Something that checks the front end.** Above. Written with the "no Node
  toolchain" rule stated as the constraint it is, because the obvious answers all
  break it.

**Handover notes for card 106**, which owns players, devices and state - all
deliberately left alone here:

- Whether the panel is on, and what address it is pointed at, are **not** in
  `StudioState`. The switch's state reaches other browsers only through the
  half-second heartbeat, and the address lives in each browser's `localStorage`. That
  is exactly the state that belongs in 106's store; when it moves, `set_panel` should
  publish like the other changes do and the address box should stop being a local
  secret.
- `AppState` is already the shape a registry wants: `Arc<Mutex<Engine>>` plus three
  channels. A player per device is another engine and another frame cell; the WS
  handler would then need to say *which* player a socket is watching (a query
  parameter, next to `?client=`).
- `Studio::bind` / `serve` / `spawn` and the `Running` guard are the lifecycle hooks:
  `stop_on_signal()` already does the SIGTERM half of "restart the container and it
  resumes", and `close_panel()` is where `FINAL` goes out.
- `StateEvent.rev` is a monotonic counter. It is enough to notice a missed change but
  not to resolve one; if 106 needs stronger ordering it should replace it rather than
  lean on it. One known edge: two changes from two browsers inside one socket's poll
  are coalesced by the broadcast only on lag, but the `from` filter means the browser
  that made the *newer* one will not be told about the older one it missed. Harmless
  for a design tool, wrong for anything that must converge.
- The engine holds a `std::sync::Mutex` across a render (several milliseconds), and
  every request takes it. Fine for one engine and a handful of requests; worth
  rethinking before there are several players.

**Nothing left running.** Every simulator was started with `--exit-after` and every
run under `timeout`; `ps` at the end of the session shows no `screeny-studio`, no
`screeny-sim`, no stray cargo belonging to this worktree. One `screeny-sim
--exit-after 400` was seen during the work and left alone: `lsof` shows its working
directory is `.claude/worktrees/agent-a49893d8d2978104f`, another worker's.

### Acceptance, against the card

| the card asked for | where it is |
|---|---|
| axum server, UI embedded at build time, `--ui-dir` override | `src/ui.rs`, `src/lib.rs`; `the_ui_is_served_from_the_binary` |
| `--listen`, default `127.0.0.1:8787` | `src/main.rs`, 3 unit tests; compose can pass `0.0.0.0:8787` |
| the 11 commands 1:1, names kept, under `/api/v1` | `src/api.rs` - 13 routes, card 101's two included; `the_api_round_trips` |
| preview frames over a WebSocket, binary, newest-wins, drop if slow | `src/ws.rs`; `the_socket_delivers_frames`, `a_stalled_browser_does_not_hold_up_the_engine_or_the_panel` |
| UI: `invoke()` -> `fetch()`/WS, two browsers in sync | `ui/main.js`; `two_browsers_see_each_others_changes`, `a_late_browser_starts_in_step` |
| Tauri, config, icons, capabilities, build script gone | deleted in step 1; the generated `gen/` directory and its `.gitignore` line too |
| dependency tree and build time before/after | step 3: 292 -> 160 crates, 296.7 -> 189.3 CPU-seconds |
| tests: API round-trips, WS frames, stalled client, two clients | 15 tests in `crates/studio` |
| bounded by construction: no unbounded channels or buffers | one-slot `watch` for frames and status, 32-deep `broadcast` for state, 3 s send timeout; measured in step 2 |
| "send to panel" to a localhost sim, sim shows what the preview shows | `send_to_panel_streams_the_preview_to_the_device` |
| no GUI system libraries needed to build | step 3; and `--no-default-features` has no graphics driver in the tree at all |
| `crates/studio` a normal workspace member; `CLAUDE.md` updated | step 3 |
| `cargo test --release --no-fail-fast` green at the root | **303 passed, 0 failed** |

Not done here, on purpose: players, devices and persistent state (106), Docker (107),
a scheduler (104), `Link::measure` (111/112). No hardware was touched and no LAN
packet was sent: every target in every run was an explicit `127.0.0.1`.

### For the orchestrator: the real-panel run, from a browser

The panel step is yours - a worker's environment cannot reach the LAN (card 101
measured that; card 110 is the cause). From the main checkout after merging:

```sh
cargo run --release -p screeny-studio
#   studio: http://127.0.0.1:8787/
```

Open that URL. The studio starts on `clocks-numerals` at 60 fps. In the inspector's
**Panel** section, type `screeny-4a00a4` into the address box and turn **Send to
panel** on; the status line under it should go from `Connecting: screeny-4a00a4.` to
`Sending to screeny-4a00a4 at ... at 30 fps.`, with `exact` climbing and `fallback` at
0 for an indexed piece. Pick pieces from the list as usual; `metaballs` is the
continuous one, where the frame-size meter should sit near 1464 bytes.

The same thing without a browser, if the picture is all you want to check:

```sh
curl -s -X POST -H 'content-type: application/json' \
     -d '{"on":true,"to":"screeny-4a00a4"}' http://127.0.0.1:8787/api/v1/set_panel
curl -s http://127.0.0.1:8787/api/v1/panel_status          # the status line, as JSON
curl -s -X POST -H 'content-type: application/json' \
     -d '{"id":"overland"}' http://127.0.0.1:8787/api/v1/set_piece
curl -s -X POST -H 'content-type: application/json' \
     -d '{"on":false,"to":""}' http://127.0.0.1:8787/api/v1/set_panel
```

and `screeny --name screeny-4a00a4 stats -n 20` for the device side, read-only, as
card 101 ran it.

Ctrl-C is safe and is now the tidy way to stop: it sends `FINAL` and the device goes
`LIVE -> HOLD` at once (measured against the simulator in step 4). Nothing here
touches serial, flash, the camera, `reboot` or `brightness`.

**Two things to expect.** `screeny-studio` is a new binary identity, so macOS may ask
for Local Network permission the first time it sends - that is card 110, and the note
appended to it records that there is no longer a bundle to worry about. And the engine
renders at 60 while the link puts 30 on the wire, so about half the offered frames
come back `coalesced`: that pair is the result, not a fault.

### Merging: what landed on main while this was being written

This branch is based on `dc61607`; cards 066, 111 and 112 have merged since. Three
notes, checked against `main` rather than guessed:

1. **Card 112 made `sender` default-on in `screeny-art`.** This crate is correct
   either way, as the brief asked: it depends on `screeny-art` with
   `default-features = false, features = ["sender"]` and forwards its own `gpu`
   feature to `screeny-art/gpu`, so it names everything it needs and inherits no
   defaults. Nothing to reconcile.
2. **Card 111 added `Link::attach` for an already-resolved device, and rewrote
   `crates/art/tests/sender.rs` to use it instead of walking a port range.**
   `crates/studio/tests/panel.rs` still walks 50700..50780 for a *consecutive* pair,
   because this worktree predates `attach` - the comment there credits card 101's
   workaround, which is now the old way. It works, and it is three lines to modernise
   once the two are in the same tree. Worth doing, not worth a card.
3. `CLAUDE.md` and the root `Cargo.toml` are untouched on `main` since this branch's
   base, so the one-sentence change to each should merge cleanly. `Cargo.lock` will
   conflict - it always does - and is regenerated by a build.

### Orchestrator: merged, and run against the real panel (2026-09-19)

Merged to `main` cleanly (`Cargo.lock` included). Removed the orphaned, formerly
gitignored `crates/studio/gen/` (Tauri's generated schemas) from the main checkout. Root
`cargo test --release --no-fail-fast`, whole workspace, studio included: 310 passed, 0 failed.

Real panel, driven over the HTTP API (`set_piece clocks-numerals`, then
`set_panel {"on":true,"to":"screeny-4a00a4"}`): `panel_status` reported `up`, 30 fps,
1265 sent / 1264 coalesced, indexed exact 1265 / fallback 0, `pal8-lz` 389-605 B; the 182
"dropped" are the ~3 s of 60 fps frames offered while mDNS resolution and the handshake were
still in progress, and the count did not move afterwards. Device `screeny stats`: LIVE, 30-31
rx/s and shown/s, stale/supers/decode/reject all 0. `SIGTERM` printed `studio: stopping;
releasing the panel` and the device went `LIVE -> HOLD`.

Not verified by the orchestrator either: the page rendered in a browser (the Chrome extension
was not connected). `GET /` serves 200; the owner opening the tab is the first real render.
Card 121 exists for exactly this gap.
