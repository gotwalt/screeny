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

### Step 3 - the acceptance tests, and four real bugs they found

`crates/studio/tests/fleet.rs`, 12 tests against `screeny-sim` on loopback. Ports
50800..50900, mDNS off, discovery off, every target an explicit `127.0.0.1`; nothing
here could reach the bench panel even in principle.

| test | what it pins |
|---|---|
| `a_typed_address_becomes_a_device_and_starts_playing` | `pending:127.0.0.1:P` -> the panel's own id `aa11bb`; the typed name survives the adoption; telemetry arrives; the player streams |
| **`the_panel_comes_back_whatever_is_restarted`** | **the card's acceptance.** Four rounds: kill the simulator; kill the server; both, server first; both, panel first. Every time the panel comes back playing `metaballs` seed 4242 at 30 fps with nobody doing anything |
| `the_design_view_resumes_where_it_was` | piece, seed, paused, speed, fps - and `panel_on`/`panel_to`, which card 105 left out of its state on purpose |
| `promoting_the_preview_is_explicit` | playing with the design view does not touch a panel's player; `adopt_preview` does |
| `a_bad_piece_is_contained_and_the_rest_carries_on` | a panicking piece and a stalling one, each replaced by the fallback, with a second panel playing throughout and never disturbed; `/healthz` stays 200 |
| `the_fault_pieces_are_not_on_the_menu` | a normal studio refuses them and does not list them |
| `a_missing_panel_is_never_unhealthy` | no panel, a panel that never existed, and a panel that vanished mid-stream: all 200 |
| `a_wedged_preview_is_a_503_and_the_dashboard_still_answers` | 503 with a reason, **and `/api/v1/status` answers in under 3 s while the engine's lock is held by a piece that will not give it back** |
| `a_state_directory_that_cannot_be_used_is_a_503` | the other 503; and the server still serves and still plays |
| `the_device_controls_reach_the_device` | brightness (asked 200, applied 120 - the simulator's cap), identify, name (on the device and here), stats, reboot; reboot without `confirm` is a 400, an unknown device a 404, an unreachable one a 409 |
| `the_brightness_policy_survives_the_panel_rebooting` | the panel comes back at its own 120 and the studio puts it back to 33 |
| `a_broken_state_file_starts_a_working_server` | corrupt, truncated and future-version files, end to end |

**Four bugs these found, all real, none cosmetic:**

1. **`Store::flush` could return before the write landed.** It waited on "is anything
   pending", so it returned in the gap between the writer taking the work and finishing
   it - and a restart then read the *previous* state. Now a queued/done pair of counters:
   `flush` waits for `done >= queued`. This is the bug that would have made "resumes
   exactly" quietly false about the last change before a restart.
2. **A device read back out of the state file was never re-resolved.** Only a
   `pending:` id triggered the `GET_INFO`, so after a restart a device had its real id,
   no resolution and therefore no control port - brightness, identify, name, stats and
   reboot would all have been 409 for ever after the first restart. Now anything
   unresolved with an address is asked.
3. **A wedged piece took the dashboard with it.** `/api/v1/status` read the engine
   directly, so the one moment somebody wants the dashboard was the one moment it
   hung. The status heartbeat now uses `try_lock` and keeps the last readable view;
   `/healthz` and `/api/v1/status` read that. `Running::stop` also grew so a restart
   test can wait for the previous server to *finish*, rather than sleeping and hoping.
4. **The brightness policy only re-applied when the link noticed a new session.** A
   panel that power-cycles quickly enough that UDP never notices comes back at full
   brightness with the link perfectly happy. The supervisor now also compares the
   device's own telemetry against *what the device said it applied* - not against what
   was asked for, which would retry for ever against a cap.

`cargo clippy -p screeny-studio --all-targets`: clean.
One card 105 test (`the_socket_carries_the_heartbeat`) was made non-flaky: it took the
first heartbeat, which can legitimately arrive between a piece being rebuilt and its
first frame. It now waits for one that carries "now playing", which is what it meant.

### Step 4 - the dashboard, and the first time any of this was rendered

`crates/studio/ui/dashboard.{html,css,js}`, served at `/dashboard`, with one link to it
from the design view's readout row. `dashboard.css` borrows `style.css` whole - the same
tokens, type and controls - and overrides only what a scrolling page of cards needs that
a fixed two-column bench did not; the design view itself gains one link and two CSS
rules and nothing else.

One card per panel: **what it plays** (piece, seed, rate, on/off), **is it healthy**
(link state and fps, frames sent/folded/lost, last codec and size, reconnects, rendered
frames and measured fps, faults; then the device's own telemetry - state, frames shown,
uptime, RSSI, drops by cause and seq gaps - and firmware and panel size), and **let me
change it** (brightness, identify, rename, reboot behind a `confirm()`, forget, and
"Play my preview"). Plus adding a panel by name or address, and a server card with
health, the state file, discovery and what the design view is doing.

Decisions worth recording:

- **It does not open the preview socket.** One `GET /api/v1/status` every two seconds,
  and polling stops when the tab is hidden. A dashboard left open on a phone must not
  cost 372 KB/s of frames it does not draw (card 120). That is also why there are no
  thumbnails yet - card 142.
- Cards are built once per device and updated in place, and anything with focus is left
  alone: rebuilding would fight a slider under a thumb.
- **Brightness**: the slider's lowest non-zero stop is 6 (`BRIGHTNESS_FLOOR`), because
  1..=5 light nothing while `applied` echoes them - card 136. One constant and one
  helper, and `tests/ui.rs` asserts it stays that way so 136 can delete it in one edit.
  The maximum is the device's own cap, which is learned the only way there is: ask for
  more than it will give and see what comes back.
- Static files only. A test asserts neither page contains `http://`, `https://`, `cdn.`,
  `unpkg` or `jsdelivr`: the box it runs on has no promise of internet.

`tests/ui.rs` (4): both pages and all four assets are served (and traversal still 404s);
the design view links to the dashboard; **every `$('#id')` and `$('.class')` the script
reaches for exists in the page**; **every `api('...')` it calls is a route the server
has**. Both of those are silent failures in a browser and loud ones here.

**Rendered in a real browser.** Unlike cards 105 and 121, the Chrome extension *was*
connected in this worker's environment. With a simulator on loopback the dashboard drew
correctly, updated live, and the controls worked; the design view rendered too. No
console errors. Three things the render caught that no test would have:

1. `attempt()`'s default message overwrote the handler's own, so "this panel caps
   brightness at 120" never reached the notice line. A handler that returns a string now
   says that instead.
2. "Frames per second" wrapped to two lines in the label column. It is "Rate".
3. Cards stretched across the whole window; they cap at 400px.

And one server-side correction it prompted: when a panel caps what was asked for, the
**stored policy becomes what the panel actually does**, so the state file and the
dashboard both say the true number rather than asking for 200 for ever and being given
120.

### Step 5 - the soak, and two more bugs

`crates/studio/tests/soak.rs`, one test, bounded by construction: a fixed deadline
(`SCREENY_SOAK_SECS`, 60 s by default), a deadline on every wait, and simulators that go
away with the test. Four kinds of round, repeated until the deadline:

| round | fault |
|---|---|
| 1 | 25% of frames never arrive, for three seconds |
| 2 | the panel is unplugged for seven seconds - past the silence watchdog - and comes back on the same address |
| 3 | the panel *moves* to another address |
| 4 | a run of operator changes: three pieces, three rates, new seeds, on both the player and the design view |

After each: the link is up again, frames are flowing again, and `/healthz` is 200. At the
end: memory, and that each of the four long-lived things is still alive - the render
thread (frames advancing, `running`), the telemetry poll (telemetry seconds old), the
state writer (writes > 0, no error) and the preview engine (`alive`, not `wedged`).

The moved-panel round is the one that needs telling. An address is a way of reaching a
panel and not a name for it, so `POST /api/v1/devices/add {to, device}` moves a device
that is already known: it keeps its id, its player and its piece, and only the way there
changes. Following a move *without* being told is what an instance name is for, and card
141 is the card for doing it without mDNS.

**Two more bugs the soak found:**

5. **A player's frame count restarted with its core.** So every piece change looked like
   a render loop that had stopped, and "nothing died" could not be asserted. The counter
   is the player's now, shared with whichever core is running.
6. **A resolution was thrown away after two seconds unheard**, which rebuilt the link and
   lost the session - churn on every brief silence, and lifetime counters that reset. How
   long a resolution may go unconfirmed is `Config::stale_after` now, two minutes by
   default: a panel rebooting is not a panel that has moved.

### Step 6 - the evidence

**Root `cargo test --release --no-fail-fast`, whole workspace: 334 passed, 0 failed.**
(310 on `main` before this card; the 24 are this card's.) `cargo clippy -p screeny-studio
--all-targets`: **no warnings at all in this crate**.

**The long soak, once, on the final build** - `SCREENY_SOAK_SECS=420`, release, under
`timeout`:

```
soak: 420 s, 90 faults in 90 rounds
soak: rss 11392 -> 12272 KiB (+880 KiB, +7.7%)
soak: rendered 10616 frames (10464 since the baseline), 330 sent to the panel, 1 reconnects
soak: panics 0, stalls 0, restarts 88, state written 179 times, telemetry 0.0 s old
```

Seven minutes, ninety injected faults - twenty-two of each kind - and the studio came
back from every one of them without being touched. **Memory grew 880 KiB in seven
minutes, and that figure is the whole test process**: the studio, twenty-three
simulators started and stopped, and the test's own HTTP client. A player leaking one
frame per tick would have been 65 MB by the end.

Two numbers that look wrong and are not:

- **330 frames sent** against 10616 rendered. `LinkStats` is per link, and the link is
  rebuilt every time the panel *moves* (twenty-two times), so this is the count since the
  last move, about fifteen seconds' worth. Across a run with no moves it tracks the
  render count as usual.
- **88 restarts.** A piece change replaces the render core, by design - a piece owns its
  own state and there is no meaningful way to carry it across - and round four changes
  the piece four times. Twenty-two rounds x four. Panics and stalls are separately zero.

The first attempt at the long run failed after 233 s: the moved-panel round walked up
the test's port band and ran off the end of it. The band wraps now. Worth recording
because it is the kind of thing a sixty-second run cannot find, which is the argument
for running the long one at all.

**Nothing left running.** Every simulator in every test goes away with its test; the
browser demo's studio and simulator were started under `timeout` and stopped by hand.
`ps` at the end of the session shows no `screeny-studio`, no `screeny-sim` and no stray
`cargo` belonging to this worktree.

### Cards written, not done (reserved range 140-144)

- **140** - a device player's composing pieces cannot be acted on (`Player::act` is a
  deliberate no-op; it needs a one-slot mailbox the render loop drains, not a lock).
- **141** - follow a panel that has moved, without being told: the broadcast `GET_INFO`
  probe (spec 5.5) matched by device id, for the deployments where mDNS is not available.
- **142** - the dashboard says what each panel plays but does not show it. Thumbnails,
  after card 120.
- **143** - the design view's engine has no stall recovery, because that means touching
  all thirteen of card 105's routes and should not be mixed into a behavioural change.
- **144** - the preview and a player can fight over the same panel (spec 7.4's source
  lock). Two candidate answers; the owner should pick.

### Acceptance, against the card

| the card asked for | where it is |
|---|---|
| device registry: mDNS + manual, keyed by stable id, a collection everywhere | `src/devices.rs`; 6 unit tests, `a_typed_address_becomes_a_device_and_starts_playing` |
| players: one per device, piece + params + seed + fps + brightness policy, through `screeny::Sender` | `src/player.rs`, `src/fleet.rs` |
| a distinct preview player, pointable at a device; promotion explicit | `Engine` stays the preview; `promoting_the_preview_is_explicit` |
| containment: panic and stall caught, logged once, replaced by a fallback | `src/player.rs`; `a_bad_piece_is_contained_and_the_rest_carries_on`, with a second panel playing throughout |
| state store: atomic, versioned, resumes exactly; corrupt/missing never a crash loop | `src/state.rs`; 7 unit tests + `a_broken_state_file_starts_a_working_server` |
| `/healthz` 200/503 and `/api/v1/status` per device | `src/health.rs`; `a_missing_panel_is_never_unhealthy`, `a_wedged_preview_is_a_503_...`, `a_state_directory_that_cannot_be_used_is_a_503` |
| device controls in the UI: brightness, identify, name, stats, reboot | `POST /api/v1/device/*` and the dashboard; `the_device_controls_reach_the_device` |
| soak: automated, bounded, fault injection, flat memory, no task death, recovery | `tests/soak.rs`; the numbers above |
| **kill and restart the simulator, the server, or both, in any order** | `the_panel_comes_back_whatever_is_restarted` - four rounds, every time |
| the dashboard | `ui/dashboard.*`, rendered in a browser; `tests/ui.rs` |
| `--state-dir` / `SCREENY_STATE_DIR` / `./.screeny-studio`, `SCREENY_LISTEN` | `src/main.rs`; `a_flag_beats_the_environment_which_beats_the_default` |
| bounded by construction | one render thread per player, one link, one control request, one browse, a one-slot mailbox for the state file, capped jittered backoff, every fault logged once |
| root `cargo test --release --no-fail-fast` green; clippy clean | 334 passed, 0 failed; 0 clippy warnings in `screeny-studio` |

**Not done here, on purpose**: a scheduler or rotation (104), multi-panel sync or
fan-out (091), auth (041), Docker (107), preview bandwidth (120), `Link::measure`.

**Additive changes outside `crates/studio`, in full**: `PartialEq` on
`screeny_art::Settings` and `LimiterSettings`; `SenderOutput::attach_deferred`. Nothing
in `crates/screeny` was touched. Nothing in `crates/demos`. No hardware, no serial, no
camera, and no LAN packet: every target in every run was an explicit `127.0.0.1`.

### For the orchestrator: the real-panel run, from a browser

The panel step is yours. From the main checkout after merging:

```sh
cargo run --release -p screeny-studio
#   studio: http://127.0.0.1:8787/
#   studio: state in .screeny-studio
```

Discovery is **on** by default now, so it will browse and find `screeny-4a00a4` within
thirty seconds - and, because there is no state file yet, **adopt it and start playing**
(`studio: no player was configured; ... will play clocks-numerals`). That is the card's
"first device found plays a default piece", and it is the first thing to check: the
panel should light up on its own, with nobody having asked for anything.

Then open **`http://127.0.0.1:8787/dashboard`**:

1. One card, named `screeny-4a00a4`, pill reading **PLAYING**. The health list should
   show `up . 30 fps`, frames climbing, `pal8-lz` or `pal4-lz`, `exact`, and the
   device's own telemetry: `live`, uptime, `-NN dBm`, `dropped: none`.
2. **Piece** - pick `metaballs`, then `overland`. The panel follows within a second.
   `overland` is a GPU piece, so this is also the check that wgpu works in this build.
3. **Rate** 30 vs 60. **Seed** and **New**.
4. **Brightness** - the slider's maximum should snap down to the firmware's cap the
   first time you ask for more than it gives, and the notice line should say so. Its
   lowest non-zero stop is 6 (card 136). Please note what the cap turns out to be: the
   simulator's is not the real one, and nothing here has ever seen the real one.
5. **Identify** - the "which one is this?" pattern for three seconds.
6. **Rename** to something, and check `screeny info --name screeny-4a00a4` shows it.
7. **Reboot** - behind a confirm. The card should go to **PANEL AWAY** and come back by
   itself, playing the same thing, with `reconnects` up by one and no operator action.
   If a brightness policy was set, it should be re-applied automatically.
8. **Play my preview** - design something in `/`, then press it: that panel starts
   playing it. Check the design view's own "Send to panel" is *not* needed for this, and
   that changing the design view afterwards does **not** change the panel.
9. From a **phone** on the LAN, with `--listen 0.0.0.0:8787`: the same page, one column.

Then the acceptance itself, which is the point of the card:

```sh
# 1. the server
kill -TERM <pid>            # 'studio: stopping; releasing the panel'; device LIVE -> HOLD
cargo run --release -p screeny-studio
# the panel comes back playing whatever it was playing, by itself

# 2. the panel
#    unplug it for ten seconds and plug it back in; watch the dashboard card

# 3. both, in either order
```

Without a browser, if the numbers are all you want:

```sh
curl -s localhost:8787/healthz                        # ok
curl -s localhost:8787/api/v1/status | python3 -m json.tool | head -60
curl -s -X POST -H 'content-type: application/json' \
     -d '{"device":"<id from status>","piece":"overland"}' localhost:8787/api/v1/player/set
curl -s -X POST -H 'content-type: application/json' \
     -d '{"device":"<id>","level":40}' localhost:8787/api/v1/device/brightness
```

and `screeny --name screeny-4a00a4 stats -n 20` for the device side, read-only.

**Two things to expect.** `screeny-studio` will ask for macOS Local Network permission
on a fresh build (card 110). And `.screeny-studio/state.json` appears in the working
directory - it is gitignored; `--state-dir` or `SCREENY_STATE_DIR` puts it elsewhere.

### For card 107 (Docker)

- **`SCREENY_STATE_DIR=/data`** and a volume there. Nothing else needs configuring; the
  studio creates the directory if it is missing and says once, on `/healthz` and
  `/api/v1/status`, if it cannot write it.
- **`SCREENY_LISTEN=0.0.0.0:8787`** works as the env fallback for `--listen`, so compose
  needs no command line at all.
- **The healthcheck.** `GET /healthz` is 200/503 now and it means something. **It does
  not go 503 because a panel is missing** - deliberately, so `restart: unless-stopped`
  never fights an unplugged panel. Read "Health: what 503 means" in
  `crates/studio/README.md` before writing the healthcheck's `retries`; the four
  conditions are all ones a restart genuinely fixes.
- `SIGTERM` already releases the panels and flushes the state file, so
  `stop_grace_period` can stay short; two seconds is plenty.
- Discovery is on unless `--no-discover`. On Linux with `network_mode: host` that is
  what you want. If avahi fights over 5353, the studio still works entirely from
  configured addresses - `/api/v1/status` reports `discovery.last_error` and stays 200.
- `TZ` matters: the clock pieces read the local time zone.

### For card 104 (runner/scheduler)

The seam is `Player::configure(&PlayerChange)` and the `StoredPlayer` it writes. A
scheduler changes a player's piece, seed and parameters and the rest already follows -
the state file, the dashboard, the fallback, the persistence. Quiet hours are
`PlayerChange { brightness }` and `{ on }` on a clock; both are already policy rather
than one-off commands, and both already survive a restart and a reconnect.

Do **not** build the scheduler inside `fleet::supervise`: that loop is the watchdog and
should stay something one can read in one screen.
