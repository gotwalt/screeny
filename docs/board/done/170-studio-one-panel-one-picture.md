---
id: 170
title: Studio - one panel, one picture; the page is a window onto the device
type: build
hardware: no
depends: [106, 165]
owner: worker-170
branch: card/170-one-panel-one-picture
---

## Goal

Make the Studio what the owner means it to be. His words (2026-09-19):

> The objective of the web application is to almost always be connected to a panel, only
> one panel, and there's almost always only ever going to be one panel on a given network.
> So when you set up the web application, you connect it to a panel, and then the web UI
> will allow you to preview it in case you don't have the panel within eyesight. We want
> the thing that's on screen to generally show what the device is also doing at the same
> time.

## Context

- Card 106 built the opposite emphasis, from an earlier reading of the vision: a *preview*
  engine behind the design view with its own piece/params/seed, a separate *player* per
  device, a "Send to panel" switch on the design view, "Play my preview" to promote, and a
  separate `/dashboard`. The two can even fight over the panel's source lock (old card 144,
  deleted - this card dissolves the question instead of answering it).
- What card 106 got right and must survive: the device registry keyed by stable id; the
  player owning the link; containment (panic/stall -> fallback); the state store and its
  recovery rules; `/healthz` semantics (a missing panel is never 503); the supervisor,
  telemetry poll and brightness policy; auto-adopting the first panel found. Read its Log
  in `docs/board/done/106-studio-players-devices-state.md`, and card 165's (per-piece
  settings memory, one shared map).
- `docs/design/studio-vision.md`, "One panel, one picture", is the requirement. The data
  model stays a collection of devices and players (the owner's decision 3: several panels
  must not be precluded); the **UI and the defaults assume one**.
- The service is live on the owner's Linux box with a v2 state file (after card 165). This
  card changes what the state means; migrate, never destroy.

## Deliverables

- **One engine per panel, and the page shows it.** The design view's canvas shows the
  attached panel's player: the same frames that go to the panel (the decoded datagram, as
  today's preview does), by the same WebSocket. Piece, params, seed, playback, pipeline
  settings: the page's controls act on that player, apply to the panel at once, and
  persist. The separate preview engine, its state, the "Send to panel" switch and "Play my
  preview" go away. Two browsers still stay in step.
- **Setup, once.** With no panel attached, the page says so and offers what it has found
  by mDNS plus "enter an address"; the first panel found is still adopted automatically
  (card 106) so the zero-click case stays zero clicks. Attached panel away (unplugged,
  rebooting): the page keeps rendering and showing the picture, says plainly that the
  panel is away, and the link comes back by itself. Changing which panel the Studio is
  attached to is possible but tucked away (it is a setup action, not a daily one).
- **Panel status and controls on the same page**, not a separate app: connection state,
  fps, drops, RSSI, firmware, uptime; brightness, identify, rename, reboot (behind a
  confirm). Fold `/dashboard` in; keep `/dashboard` as a redirect. Several panels, if
  present, get a plain chooser - nothing more.
- **A way to look without touching the panel is NOT a goal** - do not build a sandbox mode.
  The one exception worth keeping: a "panel output off" control (the panel goes to its own
  idle screen, the page keeps showing the piece), because people turn displays off.
- **Works on a phone and in a narrow window** (this absorbs card 161: the owner's first
  screenshot showed the preview overlapping the inspector at ~600 px). Check at 390, 600,
  900, 1400 px in a real browser, screenshots in the Log. Static files only, no CDN, keep
  the design language. Parameters that are named stops (card 163) can stay sliders.
- **State**: migrate the v2 file (preview + players) to the new shape: the attached panel's
  player is the truth; the old preview block's piece/params are dropped after being merged
  into the per-piece memory. Same recovery rules.
- **API**: keep `/api/v1` working for the routes the firmware session and scripts use today
  (`set_panel {"on":false|true}` to release/retake the panel, `status`, `healthz`,
  `player/set`); the old preview routes become aliases onto the attached panel's player.
  Document the surface in the README.
- Tests for all of it against `screeny-sim`; the restart/reboot acceptance from card 106
  still passes.

## Acceptance

On the deployed service: open the page on a laptop and a phone - both show what the panel
is showing; drag a slider and the panel and both pages change together; unplug the panel
and the page says it is away and keeps showing the piece; plug it in and it resumes;
restart the container and everything comes back as it was. There is no control on the page
whose effect on the panel is unclear.

## Log

### Step 0 - claimed, and the reading

Branch `card/170-one-panel-one-picture` off `main` (803fa62). Card to `doing/`.

Read, in the order the orchestrator gave: `CLAUDE.md`, `docs/README.md`, this card (twice),
`docs/design/studio-vision.md` ("One panel, one picture", "Built to be forgotten",
"Decisions"), the Logs of cards 105, 106 and 165, all of `crates/studio/` (13 source files,
6 test files, 6 UI files, the README), and cards 140, 142, 143, 166, 167, 120, 163.

The five things the earlier cards paid for, which this card must not undo:

1. `Store::flush` waits on a **queued/done pair**, not on "is anything pending" - card 106's
   bug 1, the one that made "resumes exactly" quietly false about the last change.
2. A device read back out of the state file has its real id and **no resolution**, so it must
   still be asked `GET_INFO` before there is a control port (card 106's bug 2).
3. A wedged piece must not be able to hang `/api/v1/status` (card 106's bug 3).
4. The brightness policy is re-applied both when the link sees a new session **and** when the
   device's own telemetry disagrees with *what it said it applied* (card 106's bug 4).
5. The per-piece memory is cleaned as `serde_json::Value` **before** serde sees it, so one bad
   value costs that value and never the file (card 165).

### Step 1 - the design (written before any code, as ordered)

#### The single engine

`Player` becomes the only renderer in the process. `Engine` (card 105's design-view engine)
is **deleted**. Everything the design view could do, a player can now do:

| was on `Engine` | now |
|---|---|
| `tick()` -> frame packet in `AppState::frames` | the **focused** player's render loop fills that same one-slot `watch` cell |
| `paused`, `speed` | `StoredPlayer.paused`, `StoredPlayer.speed` |
| `set_piece` / `set_param` / `set_seed` / `reset_params` / `set_settings` | `Player::configure(&PlayerChange)`, which already did all five |
| `restart()` | a one-slot pending flag the render loop drains |
| `act()` | a one-slot action mailbox the render loop drains (**card 140**, now in scope) |
| `set_panel(on, to)` | `PlayerChange { on }` plus `Player::aim(&Reach)` |
| `panel_status()` | `Player::status().panel` |

`src/engine.rs` is renamed **`src/page.rs`**: it keeps `StudioState`, `Bootstrap`,
`bootstrap()`, `RATES`, `HEADER`, `PACKET_BYTES` and the frame-packet writer, and holds no
engine. (A module called `engine` with no engine in it would be the opposite of "one
implementation of each thing".) Three test references to `screeny_studio::engine::*` move
with it.

**Why the player and not the engine.** The player is the one with the watchdog, the fallback
ladder, the fault brake and a link it owns rather than borrows. The engine had none of them -
that is card 143 - and a piece that wedges it holds a `Mutex` every route needs. A player's
core is owned by its own thread and is behind no shared lock at all, so a wedged piece cannot
block `/api/v1/status`, `/healthz` or anything else. **Card 143 is closed by construction.**

#### Changes are drained, not thrown at a new thread

Today every `Player::configure` that touches piece/seed/param/settings calls `restart_core`:
stop the thread, start a thread. That was tolerable when only the dashboard's piece dropdown
reached a player. It is not tolerable now that the design view's **sliders** act on the
player: dragging one is ~60 changes a second, and that would be ~60 threads a second and a
visible stutter on the panel.

So `Player` gains a one-slot `Pending { rebuild, params, settings, restart, action }`, newest
wins, drained by the render loop between frames:

- `rebuild` (piece or seed changed) -> a fresh `Core` on the same thread;
- `params` / `settings` -> re-read from `cfg` into the running core;
- `restart` -> rebuild the piece from its seed, reset the pipeline and the clock;
- `action` -> `piece.act(&id)`.

The thread is only replaced when a piece **panics** (its state is unsound) or when the
watchdog abandons a wedged one. `health.restarts` therefore becomes an honest fault counter
rather than "how many times somebody changed the piece" (card 106's soak reported 88 of them
for 88 piece changes).

#### Zero panels / one panel / the panel away / two panels

There is **always at least one player**. A player whose `device` is the empty string is
*unbound*: it renders, it fills the page, and it has no link.

| | what happens |
|---|---|
| **zero panels** | the unbound player renders. The page draws it and says "No panel yet", listing what mDNS has found plus a box to type an address. Nothing is sent anywhere. `/healthz` 200. |
| **a panel appears** | it is **adopted into that same player**: the player is *renamed* from `""` to the device id in place - the same thread, the same core, the same piece - and the link is aimed at it. The picture does not restart. Auto-adoption is still once per process and only while no player is bound to a device (card 106's rule). |
| **one panel** | one player: renders, sends, fills the page. |
| **the panel away** | the link drops. The core keeps rendering and the page keeps drawing; the page says "Panel away" and shows how long. The link reconnects by itself (`screeny::Link`). |
| **two panels** | two players. The page shows the **focused** one and offers a plain chooser; only the focused player fills the page's frame cell. A second discovered panel gets no player until somebody says "play here" - a panel that started playing on its own because it was plugged in would be a surprise, not a feature. |

`Players::rekey` is rewritten to rename in place instead of building a replacement `Player`,
because "adopted without the picture restarting" is exactly what a replacement would break.
It still does what card 106 needed it for: `pending:127.0.0.1:P` -> the panel's own id.

#### "Panel output off"

`on: false` now means **only** "release the link". It no longer stops the core, because the
page has to keep showing the piece. So:

- `on: false` -> `aim()` closes the link (`FINAL`, the panel falls back to its own idle
  screen), the core carries on, the page carries on.
- the core runs whenever the player exists and has not given up.

The idle rule that card 106 bought ("a panel unplugged for a month must not cost a core for a
month") survives, restated: a player renders at its configured rate when **the link is up** or
**a browser is watching**, and at `IDLE_FPS` (5) otherwise. Watchers are counted from the
frame cell's `receiver_count()`, which is the number of open preview sockets.

#### Frame rate

The player renders at `cfg.fps` (default 60) and `screeny::Link` decimates to the panel's
cadence, as it already does - a 60 fps piece into a 30 fps panel folds half its frames away by
design. The page gets newest-wins frames from the one-slot cell, so **a browser can never slow
the panel down**: card 105's `a_stalled_browser_does_not_hold_up_the_engine_or_the_panel` is
kept and now runs against the player.

#### API compatibility

Every route keeps its path and its shape. What changes is only *what it acts on*: the design
view's routes now act on the focused player instead of on a preview engine that nobody wanted.

| route | before | after | compatible? |
|---|---|---|---|
| `GET /healthz` | 200/503 | unchanged, minus the "preview engine wedged" condition (nothing is left to be it) | yes |
| `GET /api/v1/status` | `{ok, problems, uptime_s, version, state, discovery, preview, devices}` | same keys; `preview` now describes **the page's player** and gains `device` and `repaired_shown` | yes, additive |
| `GET /bootstrap` | pieces + preview state | pieces + the focused player's state | yes |
| `GET /frame` | preview packet | the focused player's newest packet | yes |
| `GET /piece_playing` | preview | focused player | yes |
| `GET /panel_status` | preview's link | focused player's link | yes |
| `POST /set_piece\|set_param\|reset_params\|set_seed\|set_settings\|set_playback\|restart` | preview | focused player | yes |
| `POST /piece_act` | `{action}` -> preview | `{action, device?}` -> focused player, or a named one (**card 140**) | yes, additive |
| `POST /set_panel {"on":false}` | preview link off | focused player `on:false` - the link is released, the page keeps drawing | **yes, exact body kept** |
| `POST /set_panel {"on":true,"to":"screeny-4a00a4"}` | preview link on | find-or-add that device, ensure its player, focus it, `on:true` | **yes, exact body kept** |
| `POST /player/set` | a device's player | unchanged, plus `paused`/`speed` | yes, additive |
| `POST /player/adopt_preview` | promote the preview | **deleted** - there is nothing to promote | no; the card orders it |
| `POST /devices/*`, `POST /device/*` | | unchanged | yes |
| `GET /dashboard` | the dashboard page | **302 to `/`** | the page is folded in |
| `GET /dashboard.{html,js,css}` | served | 404 (files deleted) | the card orders it |

The firmware session's two bodies - `{"on":false}` and `{"on":true,"to":"screeny-4a00a4"}` -
get a test of their own with exactly those bytes.

#### State: schema v3

```jsonc
{
  "version": 3,
  "devices": [ { "id", "name", "instance", "address", "manual" } ],   // unchanged
  "players": [ { "device", "on", "piece", "seed", "params", "fps",
                 "settings", "brightness", "paused", "speed" } ],      // + paused, speed
  "focus": "4a00a4",                                                   // new: which player the page shows
  "pieces": { "<piece id>": { "seed", "params" } }                     // unchanged (card 165)
}
```

`preview` is gone. `paused`, `speed` and `focus` are new. Everything else is byte-identical to
v2, so a v3 file is a v2 file with one block removed and three fields added.

**v2 -> v3 migration**, in the order it runs:

1. **The attached panel's player is the truth.** `players` is carried over unchanged, with
   `paused: false`, `speed: 1.0` by serde default.
2. **The `preview` block's piece/params/seed are merged into `pieces`** - but only where the
   memory has *nothing* for that piece, so a player's tuning is never overridden. (Card 165's
   v1 merge already put every player's values in there and made the panel win; re-running the
   preview's values over the top would undo exactly that.)
3. **`panel_on` / `panel_to` are dropped** in favour of the player's `on`. One case is not a
   drop: a v2 file with **no players** but `preview.panel_on: true` had the design view
   streaming to a panel and nothing else. That becomes a player - on the device `panel_to`
   names if it is in `devices`, otherwise unbound - carrying the preview's piece, seed,
   params, settings, fps and `on: true`, so nothing that was playing stops playing.
4. A v2 file with no players and `panel_on: false` becomes one **unbound** player seeded from
   `preview`, so the page comes back showing what the design view was showing.
5. `focus` = the first player's device.

Card 106's recovery rules are untouched: a file that cannot be parsed becomes
`state.bad.json`, a file from the future becomes `state.v<N>.json` and is never parsed, and a
bad *value* in `pieces` still costs exactly that value. **Never destroy a file you cannot
read.** The migration test uses a **real v2 file** - the live service's shape from card 165's
Log: one device `4a00a4`, one player on `overland` seed 4242, a `preview` block on
`clocks-numerals`, and a `pieces` map with four entries.

#### One page

`/dashboard` becomes a redirect to `/`; `ui/dashboard.{html,js,css}` are deleted, as are
`Engine`, `PreviewHealth`, `EngineView`, `adopt_preview` and the "Send to panel" section.

Layout, checked at **390 / 600 / 900 / 1400 px**:

- **>= 1100 px**: the bench as it is today - a CSS grid with the title block, the stage and
  the meters in the left column and the inspector on the right, the page itself not
  scrolling and the inspector scrolling. The ~600 px bug was this grid applying at every
  width: `minmax(0,1fr) 300px` left the stage 300 px, and the device (`flex: none`,
  `margin: auto`) simply overflowed it into the inspector.
- **< 1100 px**: `body` stops being that grid and becomes an ordinary scrolling column, in
  DOM order: **picture, now playing, parameters, panel**, then time, panel model, limiter,
  view, and the meters last. The stage gets a bounded height so the picture does not eat the
  phone. `.readouts` wraps (the clipped seed). The meters go to two columns below 760 px and
  one below 430 px (the ticks spilling into the parameters).

"Piece" and "Now playing" merge into one **Now playing** section - the piece name, what a
composing piece is performing and its actions, the piece list, and the seed - because with one
engine they are one question.

The **Panel** section carries everything the dashboard had: connection, fps, drops, RSSI,
firmware, uptime, reconnects; the output switch; brightness (floor 6, never above the device's
reported cap - card 136's workaround stays in one constant); identify; rename; reboot behind a
`confirm()`; and, tucked behind a disclosure, "change which panel". The panel facts come from
`GET /api/v1/status` every 2 s while the tab is visible - the same cheap read the dashboard
used - and the picture, the state and the heartbeat come from the WebSocket.

Existing design language kept: `style.css`'s tokens, type and controls are unchanged; static
files only, no Node, no CDN, no network fonts.

#### Bounded by construction

One render thread per player, one link, one control request in flight, one browse at a time,
a one-slot pending mailbox per player, a one-slot frame cell, a fixed-depth state broadcast, a
one-slot state-file mailbox, capped jittered backoff, and every fault logged once.

#### Cards this absorbs

- **140** (piece actions on a player) - built here as the action mailbox. Close.
- **142** (dashboard thumbnails) - the page *is* the panel's picture now. Close.
- **143** (preview stall recovery) - the preview engine is gone; the player has a watchdog.
  Close.
- **166** (the dashboard cannot tune a panel) - the page's parameter sliders tune the panel.
  Close.
- **167** (`state.repaired` is not shown) - one row in the panel section. Close.
- **120** (preview bandwidth) - still real, and more so now that the one page always streams.
  Edited rather than closed.

### Step 2 - the engine, and what it cost

`src/engine.rs` -> `src/page.rs`: it keeps `StudioState`, `Bootstrap`, the frame packet
and the piece list, and holds nothing that runs. `Engine`, `PreviewHealth`, `EngineView`,
`restore_preview`, `spawn_engine` and `adopt_preview` are gone.

**The player is the only renderer.** Three changes made that possible, and the second is
the one that mattered:

1. **`Core` produces the whole packet.** `Pipeline::process` already returned `preview`
   and `stats` beside `wire`; the player was throwing them away. Now the focused player
   writes `page::pack(...)` into the page's one-slot frame cell, so what the browser
   draws and what the panel is sent are the same bytes by construction. `Core::tick`
   also took `paused` and `speed` and matches the old engine's clock exactly (`t += dt`
   *before* the render, `wall` to the pipeline).
2. **Changes are drained, not thrown at a new thread.** Every `configure` that touched
   piece/seed/param/settings used to call `restart_core` - stop a thread, start a
   thread. Tolerable when only a dropdown reached a player; not tolerable now that the
   page's **sliders** do, because dragging one is sixty changes a second. A one-slot
   `Pending { rebuild, params, settings, restart, action }` is drained by the render
   loop between frames. Measured in the browser afterwards: a twelve-step drag reached
   the panel with **0** core restarts.
3. **`Players::rekey` renames in place** instead of building a replacement `Player`, so
   adopting a panel keeps the thread, the core and the piece - "without the picture
   restarting", as the card asks. A unit test asserts `Arc::ptr_eq` before and after.

Three things fell out that are worth recording:

- **`health.restarts` became an honest fault counter.** Card 106's soak reported 88
  restarts for 88 piece changes and had to explain them in its Log. This card's soak
  reports **0**.
- **A wedged piece can no longer hold anything.** A player's core is owned by its render
  thread and is behind no shared lock, so card 106's `try_lock`-and-cache dance around
  the engine went away and card 143 is closed by construction. `/healthz` lost its
  fourth 503 condition because nothing is left to be it.
- **`piece_playing` had to learn to say "not yet".** A change is applied before the
  *next* frame, so for up to one frame period the configuration says one piece and the
  core is still running another. Answering with the old piece's `Playing` put the wrong
  thing on the page - `tests/api.rs::the_api_round_trips` caught it within a minute of
  the first run. The cached answer is a `(piece, Playing)` pair now and is only used
  when the piece matches.

**`on: false` stopped meaning "stop".** It releases the link and nothing else; the core
keeps rendering because the page is still showing the piece. Card 106's idle rule
survives, restated: full rate while **the link is up or a browser is watching**,
`IDLE_FPS` otherwise. "A browser is watching" is `Screen::watchers()`, the frame cell's
`receiver_count()` - so a panel away for a month with nobody looking still costs 5 fps,
which is what that rule was bought for.

### Step 3 - state v3, and the page

Schema v3 as designed. The migration test uses the live service's v2 shape from card
165's Log (`LIVE_V2` in `state.rs`): one device `4a00a4`, one player on `overland` seed
4242, a `preview` block on `clocks-numerals`, a four-entry `pieces` map. Four migration
tests in all - the real file, a preview on a piece the memory does not know, a file whose
preview was the *only* thing playing (it becomes the player, on the device `panel_to`
named), and one with nothing to point at (it becomes an unbound player).

The page: `index.html`, `main.js`, `style.css`; `dashboard.{html,js,css}` deleted;
`/dashboard` a 307 to `/`. The layout rule is the whole of the ~600 px fix - the
two-column bench is behind `@media (min-width: 1100px)` and everything narrower is an
ordinary scrolling column in DOM order. `tests/ui.rs` guards both halves: that the
breakpoint exists and that nothing above it declares `grid-template-areas`, and that the
sections are in the order they have to stack in.

### Step 4 - the orchestrator's correction to `set_panel`

Told mid-card that on the live service `POST /api/v1/set_panel {"on":false}` answers 200
while the panel carries on receiving 30 fps, because card 106's version only switched the
*preview's* link - and that a firmware conformance suite ran against a panel it believed
it had borrowed, for 35 false failures. Two changes:

- **the answer says what happened**: `{on, device, label, panel, state}` rather than
  `null`, because a 200 meaning "I have let it go" and a 200 meaning "I am still
  streaming to it" must not look the same;
- **off is off for every player**, not only the one the page shows.

`tests/panel.rs::set_panel_hands_the_panel_over_and_takes_it_back` asserts none of this
on status codes. It asserts what the **device** sees: after `{"on":false}` the simulator
leaves `State::Live`, drops its `active_source`, and `frames_rx` does not move for two
seconds; after `{"on":true,"to":...}` it is `Live` again. Confirmed by hand in the
browser too - the simulator went `live -> hold` and stopped counting while the page kept
drawing.

### Step 5 - the flake work

Also told mid-card that `tests/fleet.rs` is flaky under load, with card 093's Log as the
worked example. The rule applied throughout: **no assertion is about a different moment
from the one it waited for.** `common/mod.rs` gains `until_json`, which polls a JSON
route until a predicate holds and hands back *the answer that satisfied it*; its timeout
message carries the last answer, so a failure on a busy machine says what the server
thought rather than only that time ran out.

Fixed with it: the telemetry race (it asserted telemetry immediately after streaming
started, but the poll is periodic and the two are not ordered); the brightness-after-
reboot race (it read a fresh simulator's brightness in a window the studio was racing to
close - it now reads the *first* simulator's before any policy exists, and asserts the
transition rather than an instant); and the two `until`-then-re-read pairs in the restart
and containment tests.

The one place a hard time bound is still the point - "status answers while a piece is
wedged" - measures against a baseline taken in the same test
(`worst < max(baseline * 25, 1 s)`), so a loaded host moves both numbers.

### Step 6 - rendered in a browser, and two bugs only that would have found

The Chrome extension was available. A `screeny-sim` on `127.0.0.1:50881/50882`
(`--no-mdns --headless`) and a studio on `127.0.0.1:8899` (`--no-discover`, a temporary
`--state-dir`, `--ui-dir` so edits reload), both under `timeout`. (After the firmware
session's merge the simulator binary also serves HTTP on a fixed 8080, so add
`--no-http` to that command from now on, or two simulators will not start side by side.)

Two bugs, both real:

1. **`showPanel` dereferenced the attached device before the first status read had
   returned**, throwing on every heartbeat (`Cannot read properties of null (reading
   'label')`). The pill now reads the half-second **heartbeat** rather than the
   two-second poll - so a panel going away shows up in half a second and the answer does
   not depend on a read that may not have happened - and every device field is guarded.
2. **A page opened in a background tab never fetched the panel facts at all.** The "do
   not poll while hidden" rule came from `dashboard.js` and is right for the repeat, but
   it also skipped the *first* read. There is now always one read at startup. This is
   how the extension found it: an extension-driven tab has `document.hidden === true`
   permanently.

The extension renders its tab at a fixed 1504 px viewport, so `resize_window` cannot
drive the breakpoints. The four widths were done in a **same-origin iframe harness**,
which gives each width a real viewport for media queries; each was checked
programmatically as well as by eye (`innerWidth`, `matchMedia('(min-width: 1100px)')`,
and `scrollWidth === clientWidth` - **no horizontal page scroll at any of them**).

| width | bench layout | screenshot |
|---|---|---|
| 390 | no | [`170-390.png`](../../research/img/170-390.png), panel section [`170-390-panel.png`](../../research/img/170-390-panel.png) |
| 600 | no | [`170-600.png`](../../research/img/170-600.png) - the picture is inside the stage; this is the overlap the owner saw |
| 900 | no | [`170-900.png`](../../research/img/170-900.png) |
| 1400 | yes | [`170-1400.png`](../../research/img/170-1400.png) |
| 600, panel unplugged | no | [`170-panel-away.png`](../../research/img/170-panel-away.png) |

Every control exercised, by clicking and dragging rather than by calling the API:

| | what happened |
|---|---|
| piece | clicking **Plasma** changed what the panel plays within a second; nine sliders rebuilt |
| a parameter | a twelve-step drag of **Scale** ended at 3.08 on the panel, `running: true`, **restarts 0** |
| panel output off | device went `live -> hold`, `frames_rx` stopped (12007 -> 12007 over 2 s), page said "The panel is on its own idle screen. The picture above is still playing here.", `/healthz` 200 |
| panel output on | `live` again, frames flowing, pill back to **On the panel** |
| panel unplugged | pill **Panel away** in both places, "Desk is away. It will pick this up again by itself when it comes back.", the frame sequence kept advancing (21056 -> 21176 in 1.2 s) |
| plugged back in | resumed by itself: `plasma`, `scale 3.08`, connected, device `live` |
| brightness | typing 3 snapped to **6** (card 136); 40 applied; asking for 200 answered "This panel caps brightness at 120." and the slider's maximum snapped to 120 |
| piece actions | **Compose another** on `clocks-numerals` changed what it is performing, on the player driving the panel, restarts 0 (card 140) |
| two tabs | tab 2 changed the piece and the seed; tab 1 followed (piece, seed, radio, the new sliders) and so did the panel |
| `/dashboard` | lands on `/` |
| server restart | `SIGTERM`, start again on the same state dir: `clocks-numerals` seed 217068 brightness 120 back on the panel in ~10 s, both tabs reconnected by themselves with no notice left on screen |

No console errors after the two fixes. Both tabs closed.

The state file it left behind is the shape the design asked for:

```jsonc
{ "version": 3,
  "devices": [ { "id": "d0ca5e", "name": "Desk", "address": "127.0.0.1:50881", ... } ],
  "players": [ { "device": "d0ca5e", "on": true, "piece": "clocks-numerals",
                 "seed": 217068, "brightness": 120, "paused": false, "speed": 1.0, ... } ],
  "focus": "d0ca5e",
  "pieces": { ... } }
```

### Step 7 - the evidence

**Root `cargo test --release --no-fail-fast`: 560 passed, 0 failed, 1 ignored.**
`cargo clippy -p screeny-studio --all-targets`: **no warnings in this crate** (the
`screeny-demos` ones are card 125's and were not touched).

**The long soak, once, on the final build** - `SCREENY_SOAK_SECS=420`, release, under
`timeout`:

```
soak: 420 s, 101 faults in 101 rounds
soak: rss 11088 -> 12720 KiB (+1632 KiB, +14.7%)
soak: rendered 10643 frames (10489 since the baseline), 175 sent to the panel, 0 reconnects
soak: panics 0, stalls 0, restarts 0, state written 203 times, telemetry 0.0 s old
```

Seven minutes, 101 injected faults, and **restarts 0** where card 106 reported 88 - the
number that says the pending mailbox does what it was built for. Memory grew 1.6 MB
across the whole test process (the studio, twenty-odd simulators started and stopped, and
the test's own client) in seven minutes; the assertion's bound is 40 MB.

**No hardware, no LAN.** No serial, no flash, no camera. Every address in every run and
every test was an explicit `127.0.0.1`; `--no-discover` on every studio and `--no-mdns`
on every simulator, so nothing could have reached `screeny-4a00a4` or `workbench.local`
even by accident.

**Changes outside `crates/studio`**: none at all. Nothing in `crates/art`, nothing in
`crates/screeny`, nothing in `crates/proto`, nothing in `firmware/`. (The card allowed
small additive changes to `crates/art` or `crates/screeny`; none turned out to be
needed, because `Pipeline::process` already returned everything the player had been
throwing away.)

### For the orchestrator: the deployed service

This is a **schema change against production data** and a **behaviour change to a route
another session drives**. The live `/data/state.json` is v2 today.

**Before the upgrade**, take a copy and note what the panel is playing:

```sh
docker exec screeny-studio cat /data/state.json > ~/screeny-backups/state-v2-$(date +%s).json
python3 -m json.tool < ~/screeny-backups/state-v2-*.json | head -40
#   "version": 2
#   players[0].piece / .seed / .params / .brightness   <- what the panel is playing
#   preview.piece    / .seed  / .params                <- what the design view was on
#   pieces{...}                                        <- the settings memory (card 165)
```

**After `tools/deploy-workbench.sh`:**

```sh
curl -s localhost:8787/healthz                         # ok
docker logs screeny-studio 2>&1 | grep 'studio: state'
#   "the state file was schema v2; migrated to v3"     <- expected, once
docker exec screeny-studio cat /data/state.json | python3 -m json.tool
```

In the new file, check:

1. **`"version": 3`**, a new top-level **`"focus"`** holding the panel's id (`4a00a4`),
   and **no `"preview"` block**.
2. `players[0]` says the same piece, seed, params, settings and brightness the v2 copy
   said, plus `"paused": false` and `"speed": 1.0`.
3. `pieces` is **unchanged** from the v2 copy, except possibly one added entry for
   whatever `preview.piece` was - and only if `pieces` had nothing for it. If an entry
   that existed in v2 has different values in v3, the migration is wrong: stop and say so.
4. `/api/v1/status` -> `state.repaired` is `[]`, and `state.bad.json` / `state.v4.json`
   do **not** exist in `/data`. Either would mean the file was not used.

Then the behaviour, in the browser at `workbench.local:8787`:

- The page is one page now. `/dashboard` redirects to it.
- The picture is the **panel's own frames**. Change the piece: the panel follows at once,
  with nothing to promote. Drag a parameter slider and watch the panel, not a preview.
- Open it on a phone as well: both show the same picture and change together.
- Unplug the panel: the page says "<name> is away. It will pick this up again by itself
  when it comes back", keeps drawing, and `/healthz` stays 200. Plug it in: it resumes.
- `docker restart screeny-studio`: everything back as it was in a few seconds.

**The `set_panel` change the firmware session needs to know about.** Its script is
unchanged - the same two bodies - but the effect is now what it always said:

```sh
curl -s -X POST -H 'content-type: application/json' \
     -d '{"on":false}' localhost:8787/api/v1/set_panel
#  -> {"on":false,"device":"4a00a4","label":"...","panel":null,"state":{...}}
#     FINAL is sent; screeny stats should show the device leave LIVE for HOLD and
#     then IDLE, and frames_rx stop moving. Before this card it answered 200 and
#     kept streaming at 30 fps.
curl -s -X POST -H 'content-type: application/json' \
     -d '{"on":true,"to":"screeny-4a00a4"}' localhost:8787/api/v1/set_panel
#  -> {"on":true,...,"panel":{"state":"connecting"|"up",...}}
```

Worth telling that session that the reply body changed shape (it was `null` / a bare
`PanelStatus`; it is now `{on, device, label, panel, state}`), and that "off" now stops
**every** player rather than only the one the page shows.

**Rolling back.** A v2 binary handed a v3 file takes card 106's from-the-future path: it
does **not** parse it, renames it to `/data/state.v3.json`, says so once, and starts from
defaults - which on that box means discovery adopts `screeny-4a00a4` and plays the
default piece. Nothing is destroyed. To undo a rollback: put the v3 image back and
`mv /data/state.v3.json /data/state.json`. To go back further than that, the v2 backup
taken above is what a v2 binary wants. (Not run against the live service; it is what
`a_state_file_from_the_future_is_kept_not_parsed` pins, unchanged by this card.)

**Running it by hand on the bench**, for anybody reproducing the browser session:

```sh
# The simulator's binary also serves HTTP on a fixed 8080 since the firmware
# session's merge, so --no-http is needed whenever two of them run side by side.
./target/release/screeny-sim --bind 127.0.0.1 --frame-port 50881 --control-port 50882 \
    --no-mdns --no-http --headless --id d0ca5e --instance sim-desk --brightness-cap 120 &
./target/release/screeny-studio --listen 127.0.0.1:8899 --no-discover \
    --ui-dir crates/studio/ui --state-dir /tmp/studio-state &
curl -s -X POST -H 'content-type: application/json' \
     -d '{"to":"127.0.0.1:50881","name":"Desk","play":true}' localhost:8899/api/v1/devices/add
```

`--ui-dir` matters: without it the binary serves the UI it was **compiled** with, which
cost half an hour of confusion in this card when an edited `main.js` appeared to change
nothing.

### Acceptance, against the card

| the card asked for | where it is |
|---|---|
| one engine per panel; the page's canvas shows the attached panel's player, by the same WebSocket | `player.rs` is the only renderer; `page::Screen` is the one frame cell; `ws.rs` unchanged. `tests/panel.rs::send_to_panel_streams_the_picture_to_the_device` compares the socket's bytes to the simulator's decode |
| piece, params, seed, playback, settings act on that player, reach the panel at once, persist | `api.rs::on_page`; `the_page_and_the_panel_are_one`; by hand, a twelve-step slider drag |
| the preview engine, its state, "Send to panel" and "Play my preview" go away | `Engine`, `PreviewHealth`, `EngineView`, `StoredPreview`, `adopt_preview` and the panel section of `index.html` all deleted; `adopt_preview` is a 404 and a test says so |
| two browsers still stay in step | `two_browsers_see_each_others_changes`, `a_second_browser_sees_the_restored_values`, and two real tabs |
| with no panel: say so, offer what mDNS found plus an address box | the unbound player, the pill's "No panel", and the chooser, which opens itself once when nothing is attached. `a_studio_with_no_panel_still_plays_something` |
| the first panel found is still adopted automatically | `fleet::adopt_first_device` -> `attach`, once per process; `a_typed_address_becomes_a_device_and_starts_playing` asserts `attached`/`focused` |
| panel away: keep rendering and showing the picture, say so plainly, come back by itself | `a_missing_panel_is_never_unhealthy`, the soak's round 2, and by hand ([`170-panel-away.png`](../../research/img/170-panel-away.png)) |
| changing which panel is possible but tucked away | a `<details>` at the bottom of the Panel section |
| panel status and controls on the same page | the Panel section: connection, fps, frames, codec, reconnects, rendered, heard, device state, uptime, RSSI, drops by cause, firmware, last error; brightness, identify, rename, reboot behind a `confirm()` |
| `/dashboard` folded in, kept as a redirect | `ui.rs::FOLDED_IN`, a 307; `the_dashboard_is_folded_in_and_redirects` |
| several panels get a plain chooser, nothing more | `Players::set_focus`, the chooser's "Use this"; `exactly_one_player_is_on_the_page` |
| no sandbox mode; "panel output off" is the one exception | `on: false` releases the link and nothing else; `set_panel_hands_the_panel_over_and_takes_it_back` asserts it on the device |
| works at 390/600/900/1400, screenshots in the Log | the table in step 6; `the_two_column_layout_is_behind_a_breakpoint` and `the_page_is_in_the_order_it_should_stack_in` keep it from being undone |
| static files, no CDN, keep the design language | three files, `the_page_keeps_its_promises` (also checks no web font from the network); `style.css`'s tokens, type and controls unchanged |
| state: migrate v2, the player is the truth, the preview's piece merged into the memory, same recovery rules | `migrate_to_v3`; four migration tests including one built from the live v2 shape |
| API: `set_panel`, `status`, `healthz`, `player/set` keep working; document the surface | the compatibility table in step 1; `crates/studio/README.md`; the exact-bodies test |
| tests against `screeny-sim`; card 106's restart/reboot acceptance still passes | `the_panel_comes_back_whatever_is_restarted`, four rounds, unchanged in substance |
| card 165's memory tests still pass | `tests/memory.rs`, 8 tests; two rewritten because the contexts they contrasted are one thing now, and said so |
| bounded by construction | one render thread per player, one link, one control request, one browse, a one-slot pending mailbox per player, a one-slot frame cell, a fixed-depth state broadcast, a one-slot state-file mailbox, capped jittered backoff, every fault logged once |
| root `cargo test --release --no-fail-fast` green; clippy clean | 560 passed, 0 failed, 1 ignored; no clippy warnings in this crate |

**Tests changed rather than kept, and why:**

| was | now | why |
|---|---|---|
| `promoting_the_preview_is_explicit` | `the_page_and_the_panel_are_one` | it pinned the *opposite* behaviour - that the design view must not touch the panel - which is the thing the owner corrected. Inverted rather than deleted, so it cannot come back |
| `a_wedged_preview_is_a_503_and_the_dashboard_still_answers` | `a_wedged_piece_is_replaced_and_the_status_route_never_stalls` | there is no preview engine to wedge. What survives is the half that mattered (status answers while a piece is stuck) plus card 143's acceptance (it recovers by itself), and `/healthz` now stays 200 throughout |
| `promoting_the_preview_needs_no_copy_step`, `the_design_view_and_the_panel_share_one_memory` | `two_panels_share_the_one_memory` | both contrasted the preview with a panel; the page *is* a panel now, so the honest version of "one memory" needs two panels |
| `the_engine_runs_with_nobody_watching` | `a_player_renders_with_nobody_watching` + `a_watching_browser_gets_the_full_rate` | with nobody watching a player idles at 5 fps on purpose, so 300 ms was not enough time to see a frame; split into the two claims |
| `a_panel_restores_a_pieces_settings_too` | same name, second half rewritten | it asserted the design view was on a *different* piece from the panel. It now asserts they are the same, which is the card |
| `the_design_view_resumes_where_it_was` | `the_page_resumes_where_it_was` | same test, plus the playback state, which used to be the preview's |

**Not done here, on purpose**: a scheduler (104), auth (041), preview bandwidth (120),
GPU-absent messaging (145), following a panel that moved without being told (141), named
parameter stops (163), the art pieces themselves.

### Cards written, not done (reserved range 171-175)

- **171** - the page's "Reconnects" count resets whenever the link is rebuilt, so a panel
  that has really reconnected three times can read 0. Card 106 wrote this down as a
  footnote; card 170's acceptance ("no readout whose meaning is unclear") makes it a bug.
- **172** - the rate control offers 30 and 60, but a player may be on any rate from 1 to
  60 and `player/set {fps}` will put it there. A panel a script set to 10 fps shows a
  control with neither stop lit. Related to card 163 - both are controls whose shape does
  not match their values.
- **173** - "No panel yet" does not say whether the studio is even looking. Discovery off,
  mDNS broken and "the first browse has not finished" all look identical, and they need
  three different things from the person reading them. `/api/v1/status` already knows.

174 and 175 are unused.

### Orchestrator: merged, deployed, verified on the live service and the real panel (2026-09-20 ~00:00 PDT)

Merged cleanly onto `main` (which by then also had cards 146/147/153 and the firmware
session's simulator HTTP work). Root `cargo test --release --no-fail-fast`: 612 passed, 0
failed, 1 ignored - the two flaky fleet tests did not recur. Looked at the 390 px and 1400 px
screenshots: stacked and two-column layouts as designed, nothing overlapping.

Live service: backed up the v2 file (`~/screeny-backups/state-v2-*.json` on workbench), pushed,
deployed. Log: `the state file was schema v2; migrated to v3` once; `repaired: []`; no
`state.bad.json`. On disk afterwards: `version: 3`, `focus: 4a00a4`, no `preview`, the player
still `overland` seed 4242 `on`, and `pieces` byte-for-byte what the v2 backup had. The panel
link was up immediately after the deploy returned; `/api/v1/status` reports the page's player
as device `4a00a4` (`attached: true`). `/dashboard` -> 307 to `/`.

`set_panel` against the real device (the call that had silently stopped working after card
106): `{"on":false}` -> reply `{on:false, device:"4a00a4"}` and `screeny stats` shows the device
go `LIVE -> HOLD`; `{"on":true,"to":"screeny-4a00a4"}` -> `LIVE` again. Firmware session told.

The owner's four open questions from the worker's report are passed on in the morning summary.
