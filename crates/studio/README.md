# screeny-studio

Screeny Studio: design generative pieces for the panel, preview them as LEDs in a
browser, and stream them to a real panel.

One ordinary Rust binary - an HTTP + WebSocket server with the UI compiled into it.
There is no desktop window and no Node toolchain; the same program runs on a laptop
and in a container ([`docs/design/studio-vision.md`](../../docs/design/studio-vision.md)).

```bash
cargo run -p screeny-studio            # http://127.0.0.1:8787/
cargo run -p screeny-studio -- --listen 0.0.0.0:8787
cargo run -p screeny-studio -- --ui-dir crates/studio/ui    # edit the UI, reload
```

| flag | |
|---|---|
| `--listen ADDR` | where to serve. Default `127.0.0.1:8787`. A bare number is a port on loopback. |
| `--ui-dir DIR` | serve `index.html`, `main.js` and `style.css` from this directory instead of from the binary, so an edit needs a reload rather than a rebuild. |

**`--listen 0.0.0.0:8787` has no password.** There is no authentication yet (parked
card 041), so anyone who can reach the port can change what is playing, and point the
studio at any panel on the network. That is the intended deployment on a home LAN or
over a tailnet, and it must not be published to the internet.

Pieces, the pipeline, the panel model and the studio's meters are documented in
[`crates/art/README.md`](../art/README.md).

## The API

Everything the UI does is one of these. Reads are `GET`, changes are `POST` with a
JSON body; the names are the ones the desktop app's IPC used.

| route | body | answer |
|---|---|---|
| `GET /api/v1/bootstrap` | | every piece and its parameters, the payload budget, the current state |
| `GET /api/v1/frame` | | one frame packet: 52-byte header + 64x32 sRGB = 6196 bytes |
| `GET /api/v1/piece_playing` | | what a composing piece is performing, or `null` |
| `GET /api/v1/panel_status` | | the panel link, or `null` when the switch is off |
| `POST /api/v1/set_piece` | `{id}` | the new state |
| `POST /api/v1/set_param` | `{id, value}` | the new state |
| `POST /api/v1/reset_params` | `{}` | the new state |
| `POST /api/v1/set_seed` | `{seed}` (`null` = a new one) | the new state |
| `POST /api/v1/set_settings` | `{settings}` | the new state |
| `POST /api/v1/set_playback` | `{paused, speed, fps}` | the new state |
| `POST /api/v1/piece_act` | `{action}` | what it is performing now |
| `POST /api/v1/restart` | `{}` | the new state |
| `POST /api/v1/set_panel` | `{on, to}` | the panel link, or `null` |
| `GET /api/v1/ws` | | the preview socket |

A failed change is a 400 with `{"error": "..."}`. `to` is an mDNS instance name
(`screeny-4a00a4`) or an address (`192.168.7.221`, `127.0.0.1:49374`); it is looked up
in the background, so turning the switch on answers at once whether or not the panel
is there.

Three kinds of message come out of the socket:

- **binary**: one frame packet, as `GET /api/v1/frame` returns;
- `{"type":"state","rev":N,"from":"<client>"|null,"state":{...}}` whenever anything
  changes, so several browsers stay in step;
- `{"type":"status","playing":...,"panel":...}` twice a second.

A browser identifies itself with an `X-Studio-Client` header on changes and
`?client=<id>` on the socket; the server does not echo a browser its own change.

## Bounded, because it is meant to be forgotten

The engine renders on its own thread whether or not a browser is connected, and
nothing a browser does can slow it or the panel link down:

- frames live in a one-slot `watch` cell that is overwritten in place - a browser that
  is not keeping up misses frames, and nothing is ever queued on its behalf;
- state changes go through a `broadcast` of fixed depth; a socket that falls behind is
  sent the current state instead of the ones it missed;
- a send that cannot complete within three seconds closes that socket.

`cargo test -p screeny-studio` proves it: with a browser wedged (a 2 KB receive
buffer, and then it stops reading) the engine still ticks 120 times and the panel link
still sends 60 frames in two seconds - 60 and 30 fps exactly.

## Editing the UI

`ui/` is three static files and no build step. They are `include_bytes!`d into the
binary, so `cargo run` always serves what is in the tree; `--ui-dir` serves them off
disk for a reload-to-see-it loop. A new file has to be listed in `src/ui.rs`.
