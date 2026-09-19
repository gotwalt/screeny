# Screeny Studio: from design tool to the thing that runs the panels

Status: **accepted** (2026-09-19). Owner's vision, orchestrator's plan; the owner's
answers to the open questions are recorded at the end and folded in throughout.

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

- `crates/art`: pieces -> limiter -> panel-aware quantise -> `WireFrame` ->
  `Output` trait. Headless binary (`list`, `pipe`, `snapshot`). Optional wgpu.
- `crates/studio`: Tauri v2 desktop app, ~1.2k lines. One `Engine` on its own thread; 11
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

- *local*: `screeny-studio` on localhost, opened in a browser tab;
- *docker*: the same binary in a container on `workbench.local`, bound to the LAN.

**There is no desktop window.** The Tauri shell is dropped (owner's decision): it is
a heavy dependency, and the real deployment is a server nobody looks at.

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

## Built to be forgotten

The owner expects this to run **for months without anyone opening the dashboard**.
That is a design requirement, not an afterthought:

- The sender reconnects by itself across panel reboots, WiFi drops and DHCP changes
  (card 011), re-resolving by mDNS name with backoff. While a panel is away, players
  keep rendering (cheaply) or idle; nothing errors out, nothing piles up.
- Players never stop on their own. A piece that panics is caught, logged, and replaced
  by the next piece / a safe fallback; the process does not die with it.
- State (what plays where, schedules, brightness policy) lives in a small file on a
  volume, written atomically; the container resumes exactly where it was after a
  restart, a host reboot or an image update. `restart: unless-stopped`.
- Bounded everything: log rotation in compose, no unbounded queues, fixed memory.
  Soak-test for leaks (hours, in CI-style tests against the simulator, not by eye).
- A `/healthz` endpoint reporting per-device `last frame sent`, `last telemetry
  heard`, fps and drops, wired to the compose healthcheck; the device's own telemetry
  (uptime, RSSI, drops by cause) is exposed so a glance answers "is it fine".
- Wall-clock pieces (the clocks) need correct time and time zone in the container
  (`TZ`, host clock via NTP).
- Quiet hours / brightness schedule belong here eventually (a panel that runs for
  months lives in a room at night).

## Deployment target: `workbench.local` (surveyed 2026-09-19)

| | |
|---|---|
| OS | Ubuntu 24.04, x86_64, 12 cores, 30 GB RAM; Docker 29 + compose v5; Portainer on :9000/:9443 |
| Network | wired, `192.168.7.6/24` - same subnet as the panel. Host avahi already resolves `screeny-4a00a4.local -> 192.168.7.221:49374`; ARP reachable. Only one WiFi hop (AP -> panel) |
| GPU | **Intel Raptor Lake-P UHD (integrated)**, `/dev/dri/renderD128`. No NVIDIA driver/runtime. More than enough for 64x32 |
| Access | SSH with keys from the bench Mac; the login user is in `docker`, `render`, `video` |

So the compose service uses `network_mode: host` (mDNS + unicast UDP just work),
passes `/dev/dri` with `group_add` for the host's `render` gid (993), and the image
carries Mesa's Vulkan driver (`mesa-vulkan-drivers`, ANV) for wgpu. No NVIDIA toolkit.
`WGPU_BACKEND=vulkan`; fall back to CPU pieces if no adapter is found and say so in
the UI. The web port must avoid what is already listening there (8000, 8443, 9000,
9443, 3002, 5002, 1080, ... ); default **8787**.

Described by a `docker-compose.yml` in the repo, deployable as a Portainer stack.
Building the image (Rust + wgpu) is slow; build on workbench over SSH
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
3. No authentication on the web UI or the device control channel (parked card 041).
   Fine on the home LAN and over the tailnet workbench is already on; do not publish
   the port to the internet.

## Repository shape

One cargo workspace under `crates/` (owner's direction), firmware outside it because
it has a different target and toolchain:

```
crates/
  proto/      no_std wire format + decoders            (firmware + host)
  receiver/   no_std receive state machine (card 016)  (firmware + sim)
  screeny/    sender library + `screeny` CLI
  sim/        fake panel          probe/   bench instrument
  art/        was crates/art: pieces, pipeline, headless bin   (pkg screeny-art)
  studio/     was crates/studio: the server + embedded UI, no Tauri    (pkg screeny-studio)
  demos/      fractal + word clock - to be ported into art as pieces, then retired
firmware/  lab/  docs/  tools/
```

With Tauri gone the studio is an ordinary server crate; `default-members` may still
leave out anything that needs wgpu so a plain `cargo test` stays fast and portable.
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
   IPC, WS preview, Tauri removed.
5. **106 players, devices, state - built to be forgotten**: device registry (mDNS +
   manual), a player per device, atomic state store, resume on restart, panic
   containment, `/healthz`, device controls in the UI. The data model is a *list* of
   devices from day one; the UI is designed around one.
6. **107 `docker-compose.yml` for workbench**: host network, `/dev/dri`, Mesa Vulkan,
   volume, healthcheck, log rotation, `TZ`; deploy over SSH; Portainer stack notes.
7. **104 runner/scheduler** (incl. quiet hours), 102 (panel model reconcile), porting
   the demos into art.

## Decisions (owner, 2026-09-19)

1. **Runs on `workbench.local`**, described by `docker-compose.yml`, managed through
   Portainer. It has a usable GPU (Intel integrated, see above).
2. **No desktop window.** Server + browser only.
3. **One panel is the expected case; several must not be precluded.** Other people may
   run this someday. So: devices and players are collections in the data model, the
   API and the state file; nothing assumes a single global device; manual addresses
   and non-host-network Docker keep working. But no multi-panel UI, synchronisation
   or fan-out optimisation is built until someone needs it (card 091 stays parked).
