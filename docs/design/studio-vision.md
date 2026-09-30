# Screeny Studio: from design tool to the thing that runs the panels

Status: **accepted** (2026-09-19). The author's vision and plan; the author's
answers to the open questions are recorded at the end and folded in throughout.

## Vision

Screeny Studio is run two ways from one codebase:

1. **Locally**, for experimenting: design patches, preview them as LEDs, push them to a
   panel on the desk.
2. **As a dockerized web app** on the network: opened in a browser from anywhere on
   the LAN, it controls what is streaming to the device(s) - which patch, which
   parameters, which panel, on what schedule - and keeps streaming whether or not a
   browser is open.

First milestone: the Studio streams to the real hardware. Then keep going.

## What exists today

- `crates/art`: patches -> limiter -> panel-aware quantise -> `WireFrame` ->
  `Output` trait. Headless binary (`list`, `pipe`, `snapshot`). Optional wgpu.
- `crates/studio`: Tauri v2 desktop app, ~1.2k lines. One `Engine` on its own thread; 11
  Tauri commands, in the names they had then (card 150 renamed them: a piece is a
  patch, and `set_settings` is `set_output`) - `bootstrap`, `frame`, `set_piece`,
  `set_param`, `reset_params`, `set_seed`, `set_settings`, `set_playback`,
  `piece_playing`, `piece_act`, `restart`; a static three-file front end whose every call goes through one
  `invoke()` wrapper that already has a non-Tauri (mock) path. The window polls
  `frame` for the newest frame.
- `crates/screeny`: sender library (card 011 is adding exact indexed sends, a push
  API and auto-reconnect), discovery, control client (brightness, identify, stats).
- The firmware arbitrates between senders itself (source lock, `BUSY`, idle fallback).

## Architecture

**Server-first.** The Studio becomes one Rust binary, `screeny-studio`, that is an
HTTP + WebSocket server (axum) embedding the static UI. Everything the UI does is an
HTTP/WS call. The two ways of running it are then the same program:

- *local*: `screeny-studio` on localhost, opened in a browser tab;
- *docker*: the same binary in a container on `studio-host.local`, bound to the LAN.

**There is no desktop window.** The Tauri shell is dropped (the author's decision): it is
a heavy dependency, and the real deployment is a server nobody looks at.

This is cheap because of how the Studio was built: the 11 commands become 11 routes,
the UI's single `invoke()` wrapper becomes `fetch()`, and frame polling becomes a
WebSocket that pushes preview frames (6 KB RGB at 30 fps is ~190 KB/s).

```
                       ┌──────────────── screeny-studio (one process) ────────────────┐
 browser(s) ──HTTP/WS──┤ api ── Players: one per panel ── patch + params + seed       │
                       │          │  render (screeny-art pipeline)                    │
                       │          ├─> screeny::Sender (exact indexed, reconnects) ────┼──UDP──> panel(s)
                       │          └─> the page's frame cell ──WS──> browsers          │
                       │             (the same decoded datagrams the panel is sent)   │
                       │ device registry: mDNS browse + manual addresses; control ops │
                       │ state store: what plays where, playlists/schedule (a volume) │
                       └───────────────────────────────────────────────────────────────┘
```

Properties that matter:

- **Streaming does not depend on a browser.** Players belong to the server. The UI is
  a remote control and a window. Restart the container and it resumes what it was
  playing (state store).
- **Channels and panels** (cards 350-355; see "Several panels" below): a channel owns
  the picture and encodes each frame once; every panel on it gets the same bytes.
- **Device controls** (brightness, identify, name, stats, reboot) are proxied through
  the existing control client. The device's own future captive-portal/HTTP settings
  page is a separate thing, for WiFi setup only.
- **One panel, one picture** (author, 2026-09-19, correcting the earlier "editing vs
  playing" split): a Studio is set up once against a panel and is then almost always
  connected to it - one panel per Studio, almost always one panel on a network. Every
  screen is a window onto what that panel is doing, for when the panel is not within
  eyesight: what is on screen is what the device is showing, at the same time. Changing a
  patch, a slider or a setting in the browser changes the panel. There is no separate
  preview stream with its own state, and no "promote to the panel" step (card 170 removed
  the one card 106 built, and folded the separate `/dashboard` into one page). Cards 198
  and 301 then split that one page into **three screens** - Picture, Panel and (since
  card 311) Settings - tied together by one nav, because "the picture", "the panel's own
  affairs" and "the studio's own setup" are three different kinds of work and reload-safe
  URLs beat tabs in one document. The data model stays a collection (decision 3 below);
  the UI assumes one. *Superseded in part on 2026-09-29 by "Several panels" below: a
  Studio now drives every panel it finds. What survives is the principle: the page is a
  live window onto what the panels are showing, not a preview with its own state.*

## Built to be forgotten

The author expects this to run **for months without anyone opening the page**.
That is a design requirement, not an afterthought:

- The sender reconnects by itself across panel reboots, WiFi drops and DHCP changes
  (card 011), re-resolving by mDNS name with backoff. While a panel is away, players
  keep rendering (cheaply) or idle; nothing errors out, nothing piles up.
- Players never stop on their own. A patch that panics is caught, logged, and replaced
  by the next patch / a safe fallback; the process does not die with it.
- State (what plays where, schedules, brightness policy) lives in a small file on a
  volume, written atomically; the container resumes exactly where it was after a
  restart, a host reboot or an image update. `restart: unless-stopped`.
- Bounded everything: log rotation in compose, no unbounded queues, fixed memory.
  Soak-test for leaks (hours, in CI-style tests against the simulator, not by eye).
- A `/healthz` endpoint reporting per-device `last frame sent`, `last telemetry
  heard`, fps and drops, wired to the compose healthcheck; the device's own telemetry
  (uptime, RSSI, drops by cause) is exposed so a glance answers "is it fine".
- Wall-clock patches (the clocks) need correct time and time zone in the container
  (`TZ`, host clock via NTP).
- ~~Quiet hours / brightness schedule belong here eventually (a panel that runs for
  months lives in a room at night).~~ Superseded first by **modes and a daily schedule**
  (card 302, 2026-09-26), then by Home Assistant taking that over entirely (card 310):
  see [Time of day is Home Assistant's](#time-of-day-is-home-assistants-cards-302-308-311).

## Time of day is Home Assistant's (cards 302, 308-311)

Card 302 (2026-09-26) built modes and a daily timetable inside the studio: the owner,
that day, *"add scheduling to the studio - i'd like, for example, to be able to go into
night mode where it's a different patch at the lowest possible visible brightness, then
restore in the morning."* Card 309 took brightness back out of a mode the same evening
(*"let's make controlling the brightness a separate concern from what's on the
screen"*) so a smart home dimming the panel would not fight the schedule. Card 308 then
put the studio on Home Assistant over MQTT discovery, with a mode standing in for an HA
scene.

Card 310 retired modes and the timetable outright: the owner, *"move all schedule
related thinking to home assistant - let's remove it from screeny studio,"* and then,
simplifying further, *"get rid of modes all together and just expose patches & settings
combinations ... that might just be simpler to think about everywhere."* The studio
keeps its named settings (card 151) - a patch on a named setting is what a mode used to
be - and exposes every patch and named setting as one `select.screeny_picture` in Home
Assistant; a schedule is now an HA automation that sets that select at the times the
owner wants, and brightness is HA's `number.screeny_brightness` or `light.screeny`, following
a room's light sensor if he likes. Card 311 then moved the Home Assistant setup itself
off the environment and onto a Settings screen in the studio (the third screen, in the
place the timetable's Schedule screen had), so there is one place - the page - to point
the studio at a broker.

How it is built - state v7 (`home_assistant`, `modes`/`schedule`/`schedule_run` dropped
and said once), the entities, the topics and the Settings screen's routes - is in
[`crates/studio/README.md`](../../crates/studio/README.md#home-assistant-cards-308-310-311).

## Several panels: channels own the picture (cards 350-355)

The owner, 2026-09-29, with a second Tidbyt on the bench: *"rethink studio so that the
same patch can be sent to one or more devices, and more than one patch can be active at
a time, eg an N:N arrangement."* Cards 350-352 built a first answer the same night and it
was wrong in its centre: the **panel** was the thing, and a channel was an implicit group
that formed when two panels picked the same picture (join / change / split rules, "Same
as", "Detach"). After using it, 2026-09-30: *"most of the time I'm going to want multiple
panels to be frame-for-frame identical. Some times I might want to have panels playing
different things. right now it's very unclear which panel is the main (or master) for a
channel, and it seems like each panel's frames are rendered independently (and sometimes
with divergent settings)."* He was right on both counts: the render was shared, but each
panel then ran its own limiter, quantise and encoder with its own output settings, so two
panels "on one channel" were not the same frames. Cards 353-355 turn it around. His
answers: a new panel **joins Channel 1**; brightness stays **per panel**; Home Assistant
picks the picture **per channel**.

**A channel is the thing.** It has an id and a name ("Channel 1", renameable), and owns
everything that decides the frames: patch, named setting, working copy (seed, params),
output settings (Dithered / Bit planes, the limiter, the panel model), the `Deck` (and its
2 s fade on a patch change), the `Pipeline` **and the encoder**. Each tick it renders,
limits, quantises and encodes **once**, and sends the **same encoded frame** to every
panel on it (only the per-link datagram header - sequence numbers - differs). Panels on
one channel are frame-for-frame identical by construction. No panel is the master.

**A panel is a member.** A device, its name, which channel it is on, on/off, and its
brightness. Brightness is applied on the device, so it never changes the frames: two
mirrored panels in different rooms can run at different levels. Every adopted panel is
on exactly one channel. Moving a panel to another channel fades that panel from the old
channel's picture to the new one's over 2 s (during that fade, and only for that panel,
its frames are its own); after that it is back on the shared bytes.

**Channels are explicit.** There is always at least Channel 1. "New channel" makes
another (starting from a copy of the current one, so the move is seamless); panels are
moved onto it by hand. A channel with no panels keeps existing and can be edited and
previewed (it renders only while someone watches it). Deleting a channel moves its
panels to Channel 1; Channel 1 cannot be deleted. Picking a picture always changes the
channel, which means every panel on it - that is the point.

**New panels** join Channel 1 and light up straight away, mirroring it.

**Encoding once.** The encoder's budget and codec choice are per link today. A channel
encodes for the most constrained of its members (smallest payload budget, the codecs all
of them accept); in practice every member is a Gen 1 Tidbyt on the same firmware, so this
is the same encoder as before, run once.

**The page.** The Picture screen is about a channel: `/?channel=<id>`. At the top, a row
of channel cards - live thumbnail, name, patch · setting, and the panels on it as chips.
Below, the editor, today's Picture screen, plus the channel's Output settings (moved here
from the Panel screen, because they now shape every member's frames) and a "Panels on
this channel" list where a panel can be moved to another channel. "New channel" sits at
the end of the row. With one channel and several panels, the page reads as today's with
"On: Kitchen, Hallway" under the title. The Panel screen is about one panel,
`/panel?panel=<device id>`: which channel it is on (a select), on/off, brightness,
identify, rename, reboot, forget, and the device block. The preview is the channel's
encoded frames decoded, so what the page shows is what every member panel shows. Phones
are first-class (card 351's touch sizing and overflow checks stay).

**API.** Picture routes (`set_patch`, `set_picture`, `set_param`, `reset_params`,
`set_seed`, `restart`, `patch_act`, `settings/*`, `set_output`, `/frame`, `/bootstrap`)
take `channel`; without one they mean Channel 1. For compatibility a `panel` on those
routes means that panel's channel. New: `GET /channels`, `POST /channels/new {name?,
from?}`, `/channels/rename {channel, name}`, `/channels/delete {channel}`,
`POST /panel/channel {panel, channel}`. `same_as` and `detach` are retired. Panel routes
(`/device/*`, `set_panel`) keep `panel`/`device`. The websocket takes `?channel=`.

**Home Assistant.** The first HA device (`screeny_<instance>`) keeps every id it has:
`select.screeny_picture` and `sensor.screeny_patch` now mean **Channel 1**, and the light,
brightness and link entities stay the first panel's. Every panel gains a **channel
select**. Every other channel is an HA device (`screeny_<instance>_ch<id>`) with a
picture select and patch sensor; every other panel keeps the device card 352 gave it
(light, brightness, link) and loses its picture and patch entities. So an automation on
`select.screeny_picture` drives every mirrored panel, and a panel's channel select is how
HA moves it.

**State v9.** Channels gain `name` and `output`; panels lose `output` and always have a
`channel`. The migration keeps v8's channels (named "Channel N" in id order), gives each
channel its first member's output, and puts idle panels on Channel 1. The v8 file is kept
as `state.v8.json`.

## Deployment target: `studio-host.local` (surveyed 2026-09-19)

| | |
|---|---|
| OS | Ubuntu 24.04, x86_64, 12 cores, 30 GB RAM; Docker 29 + compose v5; Portainer on :9000/:9443 |
| Network | wired, `192.168.1.10/24` - same subnet as the panel. Host avahi already resolves `screeny-c0ffee.local -> 192.168.1.50:49374`; ARP reachable. Only one WiFi hop (AP -> panel) |
| GPU | **Intel Raptor Lake-P UHD (integrated)**, `/dev/dri/renderD128`. No NVIDIA driver/runtime. More than enough for 64x32 |
| Access | SSH with keys from the bench Mac; the login user is in `docker`, `render`, `video` |

So the compose service uses `network_mode: host` (mDNS + unicast UDP just work),
passes `/dev/dri` with `group_add` for the host's `render` gid (993), and the image
carries Mesa's Vulkan driver (`mesa-vulkan-drivers`, ANV) for wgpu. No NVIDIA toolkit.
`WGPU_BACKEND=vulkan`; fall back to CPU patches if no adapter is found and say so in
the UI. The web port must avoid what is already listening there (8000, 8443, 9000,
9443, 3002, 5002, 1080, ... ); default **8787**.

Described by a `docker-compose.yml` in the repo, deployable as a Portainer stack.
**The files and the runbook are in [`deployment.md`](deployment.md)** (card 107);
what follows here is the reasoning behind them.
Building the image (Rust + wgpu) is slow; build on studio-host over SSH
(`docker compose build`) or publish an image, rather than building inside Portainer.

## Docker: what still bites

1. **Host networking is Linux-only.** On macOS (Docker Desktop / Colima) a container
   is inside a VM: no multicast, so no discovery; outbound unicast UDP through NAT
   does work. So the server always accepts manually configured device addresses, and
   discovery is a convenience on top. That also keeps a Mac-hosted container possible
   for other people.
2. **avahi owns UDP 5353 on the host.** The `mdns-sd` crate shares the port with
   `SO_REUSEPORT`; verify in the container early. If it fights, browse through the
   host's avahi over D-Bus instead, or rely on configured addresses.
3. No authentication on the web UI or the device control channel (a possible later addition, 041).
   Fine on the home LAN and over the tailnet studio-host is already on; do not publish
   the port to the internet.

## Repository shape

One cargo workspace under `crates/` (the author's direction), firmware outside it because
it has a different target and toolchain:

```
crates/
  proto/      no_std wire format + decoders            (firmware + host)
  receiver/   no_std receive state machine (card 016)  (firmware + sim)
  screeny/    sender library + `screeny` CLI
  sim/        fake panel          probe/   bench instrument
  art/        was crates/art: patches, pipeline, headless bin   (pkg screeny-art)
  studio/     was crates/studio: the server + embedded UI, no Tauri    (pkg screeny-studio)
  demos/      fractal + word clock - to be ported into art as patches, then retired
firmware/  lab/  docs/  tools/
```

With Tauri gone the studio is an ordinary server crate; `default-members` may still
leave out anything that needs wgpu so a plain `cargo test` stays fast and portable.
One `Cargo.lock`, one `target/` (the separate target dirs currently cost gigabytes).

## Sequence

1. **Land what is in flight**: 011 (sender embedding API) and 016 (consolidation).
   Both touch the crates that would move; reorganising under them causes conflicts.
2. **017 reorg** (mechanical, ~an hour): `git mv` into `crates/`, one
   workspace, `default-members`, fix paths/docs. Done before new patch work
   starts, so nobody builds on the old layout.
3. **101 Studio streams to hardware**: `SenderOutput` on `crates/screeny`; the engine
   gets a device target (picker: discovered + manual); real encoder stats replace the
   estimates. Milestone: design a patch in the Studio, watch it on the panel.
4. **105 server-first Studio**: axum server + embedded UI, routes replacing Tauri
   IPC, WS preview, Tauri removed.
5. **106 players, devices, state - built to be forgotten**: device registry (mDNS +
   manual), a player per device, atomic state store, resume on restart, panic
   containment, `/healthz`, device controls in the UI. The data model is a *list* of
   devices from day one; the UI is designed around one.
6. **107 `docker-compose.yml` for studio-host**: host network, `/dev/dri`, Mesa Vulkan,
   volume, healthcheck, log rotation, `TZ`; deploy over SSH; Portainer stack notes.
7. **104 runner/scheduler** (incl. quiet hours), 102 (panel model reconcile), porting
   the demos into art.

## Decisions (author, 2026-09-19)

1. **Runs on `studio-host.local`**, described by `docker-compose.yml`, managed through
   Portainer. It has a usable GPU (Intel integrated, see above).
2. **No desktop window.** Server + browser only.
3. **One panel is the expected case; several must not be precluded.** Other people may
   run this someday. So: devices and players are collections in the data model, the
   API and the state file; nothing assumes a single global device; manual addresses
   and non-host-network Docker keep working. But no multi-panel UI, synchronisation
   or fan-out optimisation is built until someone needs it (a possible later addition, 091).
