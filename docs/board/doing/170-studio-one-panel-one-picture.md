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
