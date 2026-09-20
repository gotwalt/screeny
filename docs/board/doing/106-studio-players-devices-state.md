---
id: 106
title: Studio players, devices and state - built to be forgotten
type: build
hardware: no
depends: [105]
owner: worker-106
branch: card/106-studio-players-devices-state
---

## Goal

The server owns what plays on which panel and keeps doing it for months with nobody
watching. Read "Built to be forgotten" and "Decisions" in
`docs/design/studio-vision.md`; they are the requirements.

## Deliverables

- **Device registry**: mDNS browse (`crates/screeny` discover) merged with manually
  configured addresses; devices keyed by their stable id (`id=` TXT / `GET_INFO`), not
  by IP. A *collection* everywhere (API, state file, code) even though one panel is
  the expected case; no multi-panel UI, sync or fan-out work (card 091 stays parked).
- **Players**: one per device: piece + params + seed + fps + brightness policy,
  rendering through the art pipeline into `screeny::Sender` (exact indexed frames,
  auto-reconnect from card 011). A distinct *preview* player backs the design view and
  can be pointed at a device or not; "make this what panel X plays" is explicit.
- **Containment**: a piece that panics or stalls is caught (`catch_unwind` + a
  watchdog on frame production), logged once, replaced by a safe fallback; the process
  and the other players are unaffected.
- **State store**: one small file on a volume, written atomically (temp + rename),
  versioned schema; resume exactly after restart. Corrupt/missing state -> sane
  default (first device found plays a default piece), never a crash loop.
- **Health**: `GET /healthz` (200/503) and `GET /api/v1/status`: per device last frame
  sent, last telemetry heard, fps, drops by cause, RSSI, uptime, reconnect count.
- **Device controls** in the UI via the control client: brightness, identify, name,
  stats, reboot.
- **Soak**, automated and bounded: a test binary runs the server against
  `screeny-sim` with fault injection (drops, restarts of the sim, address change) at
  accelerated time for a fixed duration and asserts flat memory, no task death, and
  recovery after every fault. Hours of wall-clock soak on workbench happen after 107,
  watched through `/healthz`, not by a worker sitting in a loop.

## Acceptance

Kill and restart the simulator, the server, or both, in any order: the panel comes
back showing what it was showing, with no operator action, every time.

## Log

### Decisions taken as given (orchestrator, recorded here as ordered)

- **State directory**: `--state-dir <DIR>`, env fallback `SCREENY_STATE_DIR`, default for a
  local run `./.screeny-studio/` (added to `.gitignore`). Card 107 mounts a volume at
  `/data` and sets `SCREENY_STATE_DIR=/data`. `SCREENY_LISTEN` is the env fallback for
  `--listen`. Flags win over env, env wins over the default.
- **`/healthz` never goes 503 because a panel is missing.** A panel that is unplugged is
  normal life; a container restart must never be the response to it. 503 is for "the
  server itself is broken".
- Reach a device **by name** when a human typed a name (so the link follows a DHCP
  lease), by **`Link::attach`** when the registry resolved it. Devices are keyed by
  stable id, never by IP.
- The dashboard is part of this card; keep `ui/`'s design language and do not restyle the
  design view. Static files only - no Node toolchain, no build step, no CDN.
- Brightness: the UI's lowest non-zero stop is the first value that lights the panel
  (1..=5 light nothing while `applied` echoes them - card 136, now scheduled by the
  firmware session). Never offer more than the device's reported cap. Kept to one
  constant and one line so card 136 can delete it.
- No auth (card 041 parked). Default listen stays `127.0.0.1:8787`.
- Per-device health has **one seam**: `TELEMETRY` over the control port, read in
  `devices.rs` and nowhere else. The firmware session's later on-device HTTP status API
  replaces the body of that one function.

### Step 1 - the state store and the device registry

**`crates/studio/src/state.rs`** - one small file, `state.json`, in the state directory.

Schema v1 (`SCHEMA_VERSION`), four parts: `version`, `devices[]`, `players[]`, `preview`.
A device is `{id, name, instance, address, manual}`; a player is
`{device, on, piece, seed, params{}, fps, settings, brightness}`; the preview is the
design view's own state plus the two things card 105 explicitly left here - `panel_on`
and `panel_to`, which until now lived only in a browser's `localStorage`.

- **Atomic**: `state.json.tmp` -> `write_all` -> `sync_all` -> `rename` over `state.json`,
  then a best-effort `fsync` of the directory. A reader sees the whole old file or the
  whole new one.
- **One writer thread with a one-slot mailbox.** `save()` never blocks and never fails;
  newest wins. A slider being dragged makes a state change per frame and costs **one**
  write. An unchanged state does not touch the disk at all (measured: two identical saves
  = 1 write).
- **Never a crash loop.** Missing, empty, truncated, corrupt, wrong-typed, no-version and
  from-the-future each start a sane default and say why *once*. The unusable file is kept,
  not deleted: an unparseable one becomes `state.bad.json` (one fixed name, so months of
  failures cannot accumulate rubble), a future-schema one becomes `state.v<N>.json` and is
  never parsed. A missing file is the first run and is not reported as a problem.
- Write failures are recorded, logged once per distinct message, and are the one state
  condition that will make `/healthz` say 503.

7 unit tests, including all four bad-file cases in one loop, the future-version file, the
no-version migration, and "20 saves leave exactly one file in the directory and the newest
one wins".

**`crates/studio/src/devices.rs`** - the registry.

Devices are keyed by **their own stable id** (`id=` TXT / `GET_INFO`), never by IP: a panel
that takes a new DHCP lease is the same panel with the same player, and the test
`a_device_that_changes_address_keeps_its_id` pins that. A manually typed address cannot be
keyed that way until the studio has spoken to it, so it gets a provisional
`pending:<what was typed>` id and **adopts its real one on the first answer** - the one
moment a key changes, and `Registry::resolved` returns the rename so the player can follow
it (`Players::rekey`). mDNS and manual addresses are merged into one list; discovery is a
convenience, never a requirement (Docker on macOS has no multicast, avahi owns 5353 on the
Linux box). `Reach` is the "how do we get there" decision in one place: `Resolved(Device)`
-> `Link::attach` (pinned to those exact two ports, card 111), `Name` -> re-resolved on
every reconnect so it follows a lease, `Addr`, or `Unknown`.

6 unit tests. Nothing in them touches the network.

**Additive in `crates/art`, and only this**: `PartialEq` on `Settings` and
`LimiterSettings` (so persisted state can be compared), and later in step 2
`SenderOutput::attach_deferred`. Both are two-line additions; nothing behavioural.

### Step 2 - players, the supervisor and the API

**`crates/studio/src/player.rs`** - one player per device, plus the fleet that holds
them.

The important structural choice: **the panel link belongs to the player, not to the
render core**. A core is the piece, its parameters and the pipeline, and nothing else.
That is what makes the stall case honest - a wedged thread cannot be killed in Rust, so
it is told to stop and *abandoned*, and what it takes with it is a piece's render state,
never a socket, a `Link`'s background thread or the device's source lock. It also fixes
card 105's handover note in passing: no mutex is held across a render on the device side.

- **panic** -> `catch_unwind` on the render thread, counted, logged once, the core is
  replaced by one running the fallback piece. The panicking thread starts its own
  replacement, so the gap is milliseconds rather than a supervisor tick.
- **stall** -> the supervisor sees no heartbeat for `WATCHDOG` (5 s), tells that core to
  stop, counts it as abandoned, and starts a fresh core on the fallback.
- **either, repeatedly** -> after `MAX_FAULTS` (3) in a row the player gives up, says so
  once, and `/healthz` goes 503. A restart loop is worse than a stopped player. A human
  asking for a piece again clears the refusal.
- `fallback_piece(avoid)` is a fixed CPU-only ladder (`plasma`, `metaballs`,
  `clocks-numerals`) so a log is predictable.
- While the panel is not connected a player renders at `IDLE_FPS` (5) instead of its
  configured rate: a panel unplugged for a month must not cost a core for a month.
- The two deliberately broken pieces (`fault-panic`, `fault-stall`) live here, are **not**
  in `screeny_art::pieces::ALL`, and are only offered when `Config::fault_pieces` is on
  (tests, or `SCREENY_STUDIO_FAULTS=1`).

**`crates/studio/src/fleet.rs`** - three slow loops, all bounded, all off the runtime
when they touch the network:

| task | period | what it does |
|---|---|---|
| supervisor | 1 s | watchdog, aims each link at its device's `Reach`, removes players whose device was forgotten, applies the brightness policy when a link comes up (on a blocking thread - a control request takes up to 1.2 s and must never be on a render thread) |
| discovery | 30 s | one browse at a time; a rename from a browse moves the player with `Players::rekey`. Off unless asked for |
| telemetry | 5 s | **the one seam** where the studio asks a device about itself (`TELEMETRY`, spec 6.7). Also does the `GET_INFO` that turns `pending:<typed>` into a real id. Capped exponential backoff with jitter per device, so a house full of panels that are all off does not poll in lockstep |

**`crates/studio/src/health.rs`** - `/healthz` and `/api/v1/status`, with what 503 means
written at the top of the file as well as in the README: the state file cannot be
written; a player has given up; a player that should be running is not (after a 15 s
start grace); the preview engine is wedged or gone. **A missing panel is never any of
them.**

**API added** (all under `/api/v1`, changes are `POST`):
`GET status`, `GET devices`, `POST devices/add|forget|refresh`,
`POST player/set`, `POST player/adopt_preview`,
`POST device/brightness|identify|name|reboot|stats`.
`set_panel` now takes a registry device id as well as a name or an address, and every
change persists. `adopt_preview` is the explicit "make what I am previewing what panel X
plays".

**`main.rs`**: `--state-dir` / `SCREENY_STATE_DIR` / `./.screeny-studio`, `SCREENY_LISTEN`
as the fallback for `--listen`, `--no-discover`, `SCREENY_STUDIO_FAULTS=1`. Flags beat
env, env beats the default - a test pins all three. `Config::default()` has discovery
**off** and `state_dir` **None** on purpose, so no test can browse the LAN or write a
file; `main` turns both on.

Measured: `cargo test -p screeny-studio` - 6 bin, 18 lib, 13 integration, all green.
Card 105's 13 tests were untouched and still pass, including the panel and stalled-browser
ones.
