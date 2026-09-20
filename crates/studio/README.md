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

**One panel, one picture.** A Studio is set up once against a panel and is then
almost always connected to it, and the page is a *window* onto what that panel is
doing - for when the panel is not within eyesight. The frames the browser draws are
the same decoded datagrams the panel is being sent, and every control on the page
changes the panel: a piece, a slider, a seed, the seconds a clock holds a time.

So there is **one page**, at `/`: the picture, what is playing, its parameters, and
the panel itself - connection, brightness, identify, rename, reboot. `/dashboard`,
which was a second app until card 170, is folded into it and redirects.

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
 state file ──> device registry ──> one player per panel ──> screeny::Link ──UDP──> panel
      ^              ^                      │   ^
      │       mDNS browse + typed addresses │   │
      │                                     │   └── supervisor (1 Hz): watchdog,
      └── every change ──────────────────── │       fallback, reconnect, brightness
                                            │
                                            └── the page's frame cell ──WS──> browsers
```

**There is one thing that renders**, and it is the player for the attached panel. The
page reads its frames out of a one-slot cell and its controls go straight to it, so
"what the browser is drawing" and "what the panel is showing" are the same bytes by
construction rather than by agreement.

- **Devices are keyed by their own stable id** (the `id=` TXT key, or what `GET_INFO`
  answers), never by IP. A panel that takes a new DHCP lease is the same panel with the
  same player. A typed address is a *way of reaching* a panel and not a name for it: a
  device added by address gets a provisional `pending:<what was typed>` id and adopts
  its real one the first time it answers.
- **One player per panel**, each rendering on its own thread. A collection from day
  one even though one panel is the expected case; several panels get a plain chooser
  and nothing more, and no multi-panel sync or fan-out is built (card 091 stays
  parked). The page shows the **focused** one.
- **A studio always has a picture**, even before it has a panel. With none found yet
  the player is *unbound*: it renders for the page and has no link. The first panel
  found is **adopted into that same player** - renamed onto it, same thread, same
  piece - so the picture the browser is watching simply starts reaching the panel
  rather than restarting on it.
- **Panel output off** (`POST /api/v1/set_panel {"on":false}`, or the switch on the
  page) releases the link with `FINAL`. The panel goes back to its own idle screen
  and **stops receiving frames**; the page carries on showing the piece. That is the
  one way to look without touching the panel, and it is deliberately the only one.
- **Changes are drained, not thrown at a new thread.** A change goes into a one-slot
  mailbox that the render loop applies before its next frame, so dragging a slider -
  sixty changes a second - costs one re-read per frame. Only a panic or a stall
  replaces a render thread, which is why `health.restarts` counts faults.
- **Reaching a panel**: by instance name when a human typed a name, which re-resolves on
  every reconnect and so follows a DHCP lease; by `Link::attach` to its exact two ports
  when the registry has resolved it.

## Built to be forgotten

The point of the whole crate. It is expected to run for months with nobody opening the
page, so:

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
devices are known, what each plays, and which one the page is showing. Written atomically
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
"version": 3,
"devices": [ { "id": "4a00a4", "name": "Desk", ... } ],
"players": [ { "device": "4a00a4", "piece": "plasma", "on": true,
               "paused": false, "speed": 1.0, ... } ],
"focus": "4a00a4",                          // which panel the page is a window onto
"pieces": {                                 // and how each piece is set, once
  "plasma":    { "seed": 111, "params": { "scale": 2.5 } },
  "metaballs": { "seed": 222, "params": { "count": 8 } }
}
```

Schema **v3** (card 170). v1 and v2 files are migrated in place, never thrown away:
v2's `preview` block - the design view's own piece, back when it had one - is merged
into `pieces` where the memory knows nothing about that piece, and dropped otherwise,
because a player's tuning must not be overwritten by a context that no longer exists.
`panel_on`/`panel_to` become the player's own `on`. A v2 file with **no** players, where
the design view was the only thing playing, becomes a player rather than losing what it
was showing.

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

503 is the three ways the *process* can be broken, each of which a restart genuinely
does fix:

1. **the state file cannot be written** - a studio that cannot save will not come back
   as itself;
2. **a player has given up** - three panics or stalls in a row, so it is no longer
   trying;
3. **a player is not running**, and has not been for longer than the fifteen-second
   start grace - a render thread that died and was not replaced.

A **missing graphics adapter is not one of them** (card 145). A studio with no GPU plays
every CPU piece perfectly well and no restart conjures one, so it is reported as `gpu` on
`/api/v1/status` and said on the page, never as a 503.

Card 106 had a fourth, "the preview engine is wedged", and card 170 deleted the thing
it was about. A piece that stops returning is now caught by the same five-second
watchdog every panel has, abandoned, and replaced by the fallback - so it is recovered
in seconds rather than waiting for somebody to restart the container. (That was card
143, closed by construction.)

`GET /api/v1/status` is the same judgement with everything behind it: per device, the
last frame sent, the last telemetry heard, fps, drops by cause, RSSI, uptime, reconnects
and what it is playing - and, since card 180, `facts`: what only the panel knows, read
from **the panel's own** `GET /api/v1/status` over HTTP. **Both answer even while a piece is wedged**, because that
is the moment somebody wants them. Card 106 had to work at this - the design view's
engine lived behind a `Mutex` that a stuck piece held for ever, so the heartbeat cached
the last readable view. A player's core is owned by its own render thread and is behind
no shared lock at all, so there is nothing left for a stuck piece to hold.

### Reading the panel's own status (card 180)

Firmware 0.4.0 and later serve an HTTP API of their own on port 80
(`docs/design/device-web.md`), and `GET /api/v1/status` there carries what UDP
telemetry cannot: heap, free stack, firmware slot and `otadata` state, why the chip last
restarted, settings-store errors, the WiFi state and SSID, and `boot_id`. The studio
reads it, merges it with the telemetry rather than replacing it, and puts it on the page
under **Device**. `boot_id` changing is the panel rebooting, and that is the only thing
reboots are counted from - never uptime.

**The panel is fragile in exactly one way, and the whole design of this is that one
rule.** It has one connection worker and no listen backlog, so a second simultaneous
connection is dropped at SYN and costs a second of retransmit. So: one poller task
(`fleet::spawn_device_http`), which `await`s each read before starting the next - one
connection in flight across the whole fleet, not one per panel; no faster than
`MIN_DEVICE_HTTP_EVERY` (10 s), which is what `Config::default` carries and what `main`
sets; `Connection: close`, a 2 s deadline over connect, write and read together, and a
4 KB ceiling on the reply (`src/devhttp.rs`, hand-written over `std::net` rather than an
HTTP client crate); capped jittered backoff; on a blocking thread, never a render one.
**A browser never triggers a read** - it gets the studio's cached copy from this
server's own `/api/v1/status`.

A panel with no HTTP server is normal - older firmware, or the portable profile pointed
at `screeny-sim --no-http`. The studio says so once, falls back to UDP telemetry alone,
asks again every two minutes in case somebody updates the firmware, and never mentions
it in `/healthz`. `--no-device-http` turns the whole thing off; `--device-http-port`
points it somewhere other than 80, which is how a simulator is talked to.

**The SSID is in that payload.** It belongs on the owner's page and in this server's
`/api/v1/status`, and nowhere else: `DeviceFacts` has a hand-written `Debug` that
redacts it, and nothing about a device is written to the state file, because these are
live facts and not state. `tests/ssid.rs` checks both.

## The API

Everything the two pages do is one of these, under `/api/v1`. Reads are `GET`, changes
are `POST` with a JSON body. A failed change is a 400 with `{"error": "..."}`; an unknown
device is a 404; a device that is known but cannot be reached right now is a **409**,
which is a fact about the panel and not a fault in the server.

### What is playing (card 105's routes; card 170 pointed them at the panel)

These are unchanged in name and shape. What changed is *what they act on*: there is no
design-view engine any more, so they configure the player for the attached panel - the
one the page is a window onto. A script written against card 105 still works, and now
it changes the panel, which is the point of card 170.

| route | body | answer |
|---|---|---|
| `GET /bootstrap` | | every piece and its parameters (a parameter that is a list of named stops carries `choices`; a 0/1 one carries `switch`), which pieces need a GPU (`needs_gpu`), the payload budget, the current state, and the adapter outcome (`gpu`) |
| `GET /frame` | | one frame packet: 52-byte header + 64x32 sRGB = 6196 bytes |
| `GET /piece_playing` | | what a composing piece is performing, or `null` |
| `GET /panel_status` | | the preview's panel link, or `null` |
| `POST /set_piece` | `{id}` | the new state |
| `POST /set_param` | `{id, value}` | the new state |
| `POST /reset_params` | `{}` | the new state |
| `POST /set_seed` | `{seed}` (`null` = a new one) | the new state |
| `POST /set_settings` | `{settings}` | the new state |
| `POST /set_playback` | `{paused, speed, fps}` | the new state. `fps` is any rate in `MIN_FPS..=MAX_FPS` (1..60), clamped there; card 172 replaced card 105's "30 or 60, anything else ignored" |
| `POST /piece_act` | `{action, device?}` | what it is performing; `device` names a panel other than the page's (card 140) |
| `POST /restart` | `{}` | the new state |
| `POST /set_panel` | `{on, to?}` | `{on, device, label, panel, state}` |
| `GET /ws` | | the frame socket |

**`set_panel` is how a script borrows the panel**, and the two bodies that matter are:

```sh
curl -s -X POST -H 'content-type: application/json' \
     -d '{"on":false}' localhost:8787/api/v1/set_panel
#  -> {"on":false,"panel":null,...}   FINAL is sent, the panel goes to its own idle
#     screen and stops receiving frames. The page carries on showing the piece.

curl -s -X POST -H 'content-type: application/json' \
     -d '{"on":true,"to":"screeny-4a00a4"}' localhost:8787/api/v1/set_panel
#  -> {"on":true,"device":"4a00a4","panel":{...},...}
```

Off is off for **every** player, not only the one the page shows. The answer says what
happened rather than `null`, because a 200 that means "I have let it go" and a 200 that
means "I am still streaming to it at 30 fps" must not look the same. (They did, until
card 170: a firmware conformance suite ran against a panel it believed it had borrowed.)

`to` is a device id, an mDNS instance name (`screeny-4a00a4`), a host name or an address
(`192.168.7.221`, `127.0.0.1:49374`); one this studio has not heard of is added, exactly
as `POST /devices/add` would. It is looked up in the background, so attaching answers at
once whether or not the panel is there. Every change is persisted.

### Panels, players and health (card 106)

| route | body | answer |
|---|---|---|
| `GET /status` | | everything: health, the state file, discovery, the graphics adapter or why there is none (`gpu`, card 145 - never a reason for a 503), what the page is showing (still keyed `preview`, for scripts written against card 106), every device |
| `GET /devices` | | the device half of `/status` on its own |
| `POST /devices/add` | `{to, name?, play?}` | `{id}` - a new panel, by name or address |
| `POST /devices/add` | `{to, device}` | `{id, moved}` - **this** panel is somewhere else now |
| `POST /devices/forget` | `{device}` | the player and the settings go with it |
| `POST /devices/refresh` | `{}` | ask every unresolved panel who it is, now |
| `POST /player/set` | `{device, on?, piece?, seed?, param?, reset_params?, fps?, paused?, speed?, settings?, brightness?, restart?}` | the player |
| `POST /device/brightness` | `{device, level}` | `{asked, applied}` - and it becomes the policy |
| `POST /device/identify` | `{device, ms?}` | |
| `POST /device/name` | `{device, name}` | renames it here, and on the device when it can be reached |
| `POST /device/reboot` | `{device, confirm}` | `confirm: true` is required |
| `POST /device/stats` | `{device}` | telemetry, read now rather than from the poll |

Three kinds of message come out of the frame socket:

- **binary**: one frame packet, as `GET /api/v1/frame` returns - the attached panel's,
  and the same bytes it is being sent;
- `{"type":"state","rev":N,"from":"<client>"|null,"state":{...}}` whenever anything
  changes, so several browsers stay in step;
- `{"type":"status","playing":...,"panel":...}` twice a second.

A browser identifies itself with an `X-Studio-Client` header on changes and
`?client=<id>` on the socket; the server does not echo a browser its own change.

Subscribing to the socket is also how the server knows somebody is watching: a player
whose panel is away and whose page nobody has open drops to 5 fps rather than rendering
60 for a month. Nothing a browser does can slow a player down - the frame cell has one
slot and the render loop never waits for a reader.

**Brightness** is a policy, not a one-off: it is re-applied whenever the link comes back,
and whenever the panel's own telemetry disagrees with what it last said it applied - a
panel that power-cycles faster than UDP notices comes back at full brightness otherwise.
The answer is what the device *applied*, which its own cap may make lower than what was
asked; the policy is then lowered to match, so the studio does not ask for something the
panel will not give. The page's slider maximum is that cap, and its lowest non-zero
stop is 6, because values 1..=5 light nothing on this firmware (card 136).

## Editing the UI

`ui/` is three static files and no build step: `index.html`, `main.js` and `style.css`.
They are `include_bytes!`d into the binary, so `cargo run` always serves what is in the
tree; **`--ui-dir` serves them off disk** for a reload-to-see-it loop, which is what to
use while editing. A new file has to be listed in `src/ui.rs`.

The layout is a **scrolling column by default** - picture, now playing, parameters,
panel - and becomes the two-column bench only above 1100 px, where there is room for
both. Doing it the other way round is what used to put the walnut frame on top of the
controls at around 600 px. Checked at 390, 600, 900 and 1400 px in a browser; the
screenshots are in card 170's Log and, for the controls below, in cards 145/163/171-173's.

**A control's shape comes from what it controls** (card 163). The page builds each
parameter from its `ParamSpec`: an ordinary number is a slider, a spec with `choices` is
a segmented control (three stops or fewer, which fit across the inspector at 390 px) or a
`<select>` (more than three), and a spec with `switch` is a switch. Nothing about the
value changes - it is an `f32` set with `set_param` either way - so a piece asks for the
control it wants by how it declares the parameter, and never by putting a key in a label.

**Nothing on the page may say something that is not so.** The rate control spans the
player's whole range rather than offering two stops it might not be on (172); the panel
section says whether the studio is even looking for panels (173); "Reconnects" is a
player-lifetime count that survives the link being rebuilt (171); and a piece that needs
a graphics adapter there is none for is struck through with the reason rather than
offered and then black (145).

No framework, no bundler, no CDN: the box this runs on has no promise of internet, and a
test asserts that neither page reaches outside it.

## Tests

`cargo test -p screeny-studio`. Nothing touches the bench device or the LAN: every
target is an explicit `127.0.0.1`, discovery is off in `Config::default()` on purpose,
and `state_dir` is `None` there too so a test cannot leave a file behind.

| file | what it pins |
|---|---|
| `tests/api.rs` | the page's routes, the frame socket, two browsers in step, the heartbeat, a frame packet's shape, and a studio with no panel at all |
| `tests/panel.rs` | what the browser draws is what `screeny-sim` shows, byte for byte; a stalled browser holding up neither a player nor the link; **`set_panel` really hands the panel over and takes it back**, asserted on what the device sees; and a panel stopped and started twice reading **2 reconnects**, across a link rebuild (171) |
| `tests/fleet.rs` | devices, players, containment, health, the device controls - and **the card's acceptance**: kill the simulator, the server, or both in either order, and the panel comes back playing what it was playing |
| `tests/soak.rs` | a bounded soak at accelerated time: frame loss, the panel going away, the panel moving, a run of changes; flat memory, nothing dead, recovery after every fault. `SCREENY_SOAK_SECS` lengthens it |
| `tests/device_status.rs` | card 180: with the panel's HTTP API on, the page has heap, free stack, slot and WiFi beside the UDP telemetry; with it off, nothing complains and `/healthz` stays 200; a simulator restarted on the same ports is counted as **one** reboot, from `boot_id`; **at most one connection open to a device at a time**, measured by a server that counts them; and a reply that never ends is refused rather than read |
| `tests/ssid.rs` | the network name is on `/api/v1/status`, where the page needs it, and in neither the studio's log (checked by running the real binary as a subprocess and reading its stderr) nor `state.json` |
| `tests/ui.rs` | the page and its two files are served, `/dashboard` redirects, every element the script reaches for exists, every route it calls exists, the narrow layout stays the default - and, since the truth-telling cards, that the page can say whether discovery is on (173), that the adapter outcome is on both routes and is never a 503 (145), that the rate slider spans `MIN_FPS..=MAX_FPS` and a rate a script set is what the page reports (172), and that a parameter with named stops carries them (163) |
| `tests/memory.rs` | card 165: switch away and back, on the page and on a panel; a second browser sees the restored values; two panels share one memory; Reset stays reset; **a fresh process on the same state directory restores a piece that is not the one showing**; a hand-edited file with garbage values; a v1 file |
| `src/*` unit tests | the state file's six failure modes, the registry's keying, the player's configuration, the argument and environment precedence |
