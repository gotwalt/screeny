# screeny-studio

Screeny Studio: the thing that plays generative art on the panels, and the browser
UI for designing it.

One ordinary Rust binary - an HTTP + WebSocket server with the UI compiled into it.
There is no desktop window and no Node toolchain; the same program runs on a laptop
and in a container ([`docs/design/studio-vision.md`](../../docs/design/studio-vision.md)).

```bash
cargo run --release -p screeny-studio       # http://127.0.0.1:8787/
cargo run --release -p screeny-studio -- --listen 0.0.0.0:8787 --state-dir /data
cargo run -p screeny-studio -- --ui-dir crates/studio/ui    # edit the UI, reload
```

Use `--release` for anything that streams: a debug build's encoder will not hold 30 fps.

Two pages:

| | |
|---|---|
| **`/`** | the design view. One piece, previewed as LEDs, with every parameter, the seed, the limiter and the panel model to play with. |
| **`/dashboard`** | the panels. What each one is playing, whether it is well, and how to change it - from a phone. |

| flag | | env |
|---|---|---|
| `--listen ADDR` | where to serve. A bare number is a port on loopback. | `SCREENY_LISTEN` |
| `--state-dir DIR` | where what-plays-where is kept. Default `./.screeny-studio`. | `SCREENY_STATE_DIR` |
| `--ui-dir DIR` | serve the UI off disk instead of from the binary. | |
| `--no-discover` | do not browse for panels; use configured addresses only. | |
| | offer two pieces that misbehave on purpose. | `SCREENY_STUDIO_FAULTS=1` |

A flag beats the environment; the environment beats the default.

**`--listen 0.0.0.0:8787` has no password.** There is no authentication yet (parked
card 041), so anyone who can reach the port can change what is playing, and point the
studio at any panel on the network. That is the intended deployment on a home LAN or
over a tailnet, and it must not be published to the internet.

Pieces, the pipeline, the panel model and the studio's meters are documented in
[`crates/art/README.md`](../art/README.md).

## What it is made of

```
 state file ──> device registry ──> one player per device ──> screeny::Link ──UDP──> panel
      ^              ^                      ^
      │       mDNS browse + typed addresses │
      │                                     │
      └── every change ──  supervisor (1 Hz): watchdog, fallback, reconnect, brightness

 the design view's own preview player ── WebSocket ──> browsers
```

- **Devices are keyed by their own stable id** (the `id=` TXT key, or what `GET_INFO`
  answers), never by IP. A panel that takes a new DHCP lease is the same panel with the
  same player. A typed address is a *way of reaching* a panel and not a name for it: a
  device added by address gets a provisional `pending:<what was typed>` id and adopts
  its real one the first time it answers.
- **One player per device**, each rendering on its own thread. A collection from day
  one even though one panel is the expected case; no multi-panel UI, sync or fan-out
  (card 091 stays parked).
- **The design view is separate.** Its preview player can be pointed at a panel, but
  nothing it does reaches a *player* until somebody says so: "Play my preview" on the
  dashboard, or `POST /api/v1/player/adopt_preview`.
- **Reaching a panel**: by instance name when a human typed a name, which re-resolves on
  every reconnect and so follows a DHCP lease; by `Link::attach` to its exact two ports
  when the registry has resolved it.

## Built to be forgotten

The point of the whole crate. It is expected to run for months with nobody opening the
dashboard, so:

**Nothing grows without bound.** Preview frames live in a one-slot `watch` cell, state
changes in a fixed-depth broadcast, and a socket that cannot take a message in three
seconds is closed. There is one render thread per player, one link, one control request
in flight, one browse at a time, a one-slot mailbox in front of the state file, and
every fault is logged *once* rather than once a frame. A device that does not answer is
polled on capped, jittered backoff. While a panel is away its player renders at 5 fps
instead of 60: a panel unplugged for a month must not cost a core for a month.

**A bad piece cannot take the process down.** A piece that panics is caught
(`catch_unwind`), logged once and replaced by a safe fallback in milliseconds. A piece
that *stalls* - a frame that never comes back - is caught by a five-second watchdog: the
wedged thread is told to stop and abandoned (no thread can be killed in Rust) and a fresh
core takes over. The panel link belongs to the player and not to the core, so abandoning
one leaks a piece's render state and one thread, never a socket, a link thread or the
panel's source lock. Three faults in a row and the player stops trying and says so:
a restart loop is worse than a stopped player.

**The state file.** One small JSON file, `state.json`, in the state directory: what
devices are known, what each plays, and the design view's own state. Written atomically
(temp file, `fsync`, rename) by one thread, newest-wins, and not written at all when
nothing has changed. It is versioned, and a version this build does not understand is
moved aside rather than parsed or deleted. **Missing, empty, truncated, corrupt,
wrong-typed or from the future all start a sane default and say why once** - a state
file is never a reason for the server not to run.

**What each piece was left set to** (card 165, schema v2) is in that same file and
**nowhere else**: no second file, nothing in the working directory, nothing in the
browser's `localStorage`. That matters operationally - the container mounts a volume at
`SCREENY_STATE_DIR` and only what is written there survives an image rebuild - and it is
why switching pieces and switching back gives you what you had, before and after a
`docker restart`.

```jsonc
"preview": { "piece": "metaballs", ... },   // what the design view is showing
"players": [ { "device": "4a00a4", "piece": "plasma", ... } ],
"pieces": {                                 // and how each piece is set, once
  "plasma":    { "seed": 111, "params": { "scale": 2.5 } },
  "metaballs": { "seed": 222, "params": { "count": 8 } }
}
```

There is **one** memory for the whole studio, not one per context: tuning a piece
anywhere updates it, switching to a piece anywhere restores from it. (The card asked for
one per context; the orchestrator reversed that on 2026-09-19 because the browser is
meant to be a window onto what the panel is doing, and card 170 unifies the preview and
the player into one engine. A per-context memory would have been built for a distinction
that is about to go away.) Which piece is showing where is still per context - the design
view and a panel can be on different pieces - it is only *how a piece is set* that is one
fact.

**Only what differs from the piece's defaults is stored**, so a later release's better
default still reaches everybody who never moved that slider, and the file stays small.
The seed is remembered with the parameters; the pipeline `settings` (levels, dither,
limiter) are not - those are about the panel, not about the piece.

**A remembered value can never break a piece.** Pieces gain, lose and re-range
parameters between releases, so every value is checked against this build's own spec on
the way in, one value at a time: a parameter that has gone away is ignored, one outside
the range is clamped to it (which is what the slider would do), and one that is not a
finite number at all goes back to the piece's default. One bad value never costs the
rest of that piece's memory, and - this is the part worth stating, because it is the
difference between a repair and card 106's `state.bad.json` - it never costs the rest of
the file. An entry for a piece this build has not got is **kept**, so a piece that comes
back in a later release comes back set up the way it was left; at most 64 such entries
are kept, so a hand-edited file cannot grow for ever. Whatever had to be corrected is
said once, on the way in, and appears under `state.repaired` in `/api/v1/status`. It is
not a fault and never a 503.

"Reset" (`POST /reset_params`, or `player/set {reset_params: true}`) means *back to the
defaults and stay there*: it forgets that piece's parameters rather than handing them
back on the next switch. It leaves the seed alone, which is what Reset is about.

A **v1** state file - what a service deployed before this card has - is migrated: what
the design view and each panel were playing is merged into the one map, so nobody loses
the tuning they have. Where the design view and a panel were on the same piece with
different values, **the panel's win**: the panel is what was being looked at.

**Stopping is clean.** Ctrl-C and `SIGTERM` (what `docker stop` sends) release every
panel with `FINAL` and flush the state file, rather than leaving the panels on the last
frame until their stream timeout.

## Health: what 503 means

`GET /healthz` is **200 `ok`**, or **503** and the reasons in words.

**It is about the server, not about the panels.** A panel that is unplugged, switched
off, rebooting or on the wrong side of a dead access point is normal life for something
that runs for months, and restarting the container is never the right answer to it. A
missing panel, a link that is `connecting`, a device that has not been heard from and an
empty device list are all **200**.

503 is the four ways the *process* can be broken, each of which a restart genuinely does
fix:

1. **the state file cannot be written** - a studio that cannot save will not come back
   as itself;
2. **a player has given up** - three panics or stalls in a row, so it is no longer
   trying;
3. **a player that should be running is not**, and has not been for longer than the
   fifteen-second start grace - a render thread that died and was not replaced;
4. **the preview engine is wedged or gone** - no frame for longer than twice the
   watchdog.

`GET /api/v1/status` is the same judgement with everything behind it: per device, the
last frame sent, the last telemetry heard, fps, drops by cause, RSSI, uptime, reconnects
and what it is playing. **Both answer even when the design view's engine is wedged**,
because that is the moment somebody wants them: the half-second heartbeat takes the
engine's lock with `try_lock` and keeps the last readable view, and these two routes read
that rather than the engine.

## The API

Everything the two pages do is one of these, under `/api/v1`. Reads are `GET`, changes
are `POST` with a JSON body. A failed change is a 400 with `{"error": "..."}`; an unknown
device is a 404; a device that is known but cannot be reached right now is a **409**,
which is a fact about the panel and not a fault in the server.

### The design view (card 105, unchanged)

| route | body | answer |
|---|---|---|
| `GET /bootstrap` | | every piece and its parameters, the payload budget, the current state |
| `GET /frame` | | one frame packet: 52-byte header + 64x32 sRGB = 6196 bytes |
| `GET /piece_playing` | | what a composing piece is performing, or `null` |
| `GET /panel_status` | | the preview's panel link, or `null` |
| `POST /set_piece` | `{id}` | the new state |
| `POST /set_param` | `{id, value}` | the new state |
| `POST /reset_params` | `{}` | the new state |
| `POST /set_seed` | `{seed}` (`null` = a new one) | the new state |
| `POST /set_settings` | `{settings}` | the new state |
| `POST /set_playback` | `{paused, speed, fps}` | the new state |
| `POST /piece_act` | `{action}` | what it is performing now |
| `POST /restart` | `{}` | the new state |
| `POST /set_panel` | `{on, to}` | the preview's panel link, or `null` |
| `GET /ws` | | the preview socket |

`to` is a device id, an mDNS instance name (`screeny-4a00a4`) or an address
(`192.168.7.221`, `127.0.0.1:49374`); it is looked up in the background, so turning the
switch on answers at once whether or not the panel is there. Every change is persisted.

### Panels, players and health (card 106)

| route | body | answer |
|---|---|---|
| `GET /status` | | everything: health, the state file, discovery, the design view, every device |
| `GET /devices` | | the device half of `/status` on its own |
| `POST /devices/add` | `{to, name?, play?}` | `{id}` - a new panel, by name or address |
| `POST /devices/add` | `{to, device}` | `{id, moved}` - **this** panel is somewhere else now |
| `POST /devices/forget` | `{device}` | the player and the settings go with it |
| `POST /devices/refresh` | `{}` | ask every unresolved panel who it is, now |
| `POST /player/set` | `{device, on?, piece?, seed?, param?, reset_params?, fps?, settings?, brightness?}` | the player |
| `POST /player/adopt_preview` | `{device}` | the player - "play what I am previewing" |
| `POST /device/brightness` | `{device, level}` | `{asked, applied}` - and it becomes the policy |
| `POST /device/identify` | `{device, ms?}` | |
| `POST /device/name` | `{device, name}` | renames it here, and on the device when it can be reached |
| `POST /device/reboot` | `{device, confirm}` | `confirm: true` is required |
| `POST /device/stats` | `{device}` | telemetry, read now rather than from the poll |

Three kinds of message come out of the preview socket:

- **binary**: one frame packet, as `GET /api/v1/frame` returns;
- `{"type":"state","rev":N,"from":"<client>"|null,"state":{...}}` whenever anything
  changes, so several browsers stay in step;
- `{"type":"status","playing":...,"panel":...}` twice a second.

A browser identifies itself with an `X-Studio-Client` header on changes and
`?client=<id>` on the socket; the server does not echo a browser its own change.

**Brightness** is a policy, not a one-off: it is re-applied whenever the link comes back,
and whenever the panel's own telemetry disagrees with what it last said it applied - a
panel that power-cycles faster than UDP notices comes back at full brightness otherwise.
The answer is what the device *applied*, which its own cap may make lower than what was
asked; the policy is then lowered to match, so the studio does not ask for something the
panel will not give. The dashboard's slider maximum is that cap, and its lowest non-zero
stop is 6, because values 1..=5 light nothing on this firmware (card 136).

## Editing the UI

`ui/` is six static files and no build step: `index.html`, `main.js` and `style.css` for
the design view, and `dashboard.html`, `dashboard.js` and `dashboard.css` for the
dashboard, which borrows the first one's tokens. They are `include_bytes!`d into the
binary, so `cargo run` always serves what is in the tree; `--ui-dir` serves them off disk
for a reload-to-see-it loop. A new file has to be listed in `src/ui.rs`.

No framework, no bundler, no CDN: the box this runs on has no promise of internet, and a
test asserts that neither page reaches outside it.

## Tests

`cargo test -p screeny-studio`. Nothing touches the bench device or the LAN: every
target is an explicit `127.0.0.1`, discovery is off in `Config::default()` on purpose,
and `state_dir` is `None` there too so a test cannot leave a file behind.

| file | what it pins |
|---|---|
| `tests/api.rs` | the design view's routes, the preview socket, two browsers in step, the heartbeat, a frame packet's shape |
| `tests/panel.rs` | "send to panel" into `screeny-sim`, byte for byte; a stalled browser holding up neither the engine nor the link |
| `tests/fleet.rs` | devices, players, containment, health, the device controls - and **the card's acceptance**: kill the simulator, the server, or both in either order, and the panel comes back playing what it was playing |
| `tests/soak.rs` | a bounded soak at accelerated time: frame loss, the panel going away, the panel moving, a run of changes; flat memory, nothing dead, recovery after every fault. `SCREENY_SOAK_SECS` lengthens it |
| `tests/ui.rs` | both pages are served, every element the dashboard reaches for exists, and every route it calls exists |
| `tests/memory.rs` | card 165: switch away and back in the design view and on a panel; a second browser sees the restored values; one memory shared by the browser and the panel; Reset stays reset; promoting the preview needs no copy; **a fresh process on the same state directory restores a piece that is not the one showing**; a hand-edited file with garbage values; a v1 file |
| `src/*` unit tests | the state file's six failure modes, the registry's keying, the player's configuration, the argument and environment precedence |
