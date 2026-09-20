---
id: 165
title: Studio remembers each piece's settings - switch away and back, and they are restored
type: build
hardware: no
depends: [106]
owner: worker-165
branch: card/165-per-piece-settings-memory
---

## Goal

The owner's request (2026-09-19): "changing settings for a given art piece persists the
settings so that if we switch pieces and then switch back, it restores the settings (with
error handling to defaults)."

## Context

- Today `Engine::set_piece` (`crates/studio/src/engine.rs`) does
  `self.params = Params::defaults(def.params)`, and a device player's piece change
  (`crates/studio/src/player.rs`, `PlayerChange`) likewise starts from defaults. The state
  file (`crates/studio/src/state.rs`, schema v1, card 106) stores one `params` map for the
  *current* piece of the preview and of each player, so tuning `clocks-numerals`, looking at
  `plasma`, and coming back loses the tuning - and so does a restart after switching.
- Card 106's state store is the place: atomic writes, one-slot writer, versioned schema,
  and a recovery path (`state.bad.json`, `state.v<N>.json`) for files it cannot use. Read
  its Log in `docs/board/done/106-studio-players-devices-state.md` first.
- Pieces change between releases: params get added, removed, renamed, re-ranged (card 160
  is adding a `rest` param to `clocks-numerals` right now). A remembered value must never
  be able to break a piece or the server.

## Decisions (orchestrator; record them in the Log)

- **What is remembered, per piece id**: the param values, and the seed. Not the pipeline
  `settings` (levels, dither, limiter) - those are about the panel, not the piece - and not
  playback state.
- **Scope**: one memory per *context* - the design view (preview) has one, and each device
  player has its own - because "what panel X plays" and "what I am fiddling with" are
  different things (card 106 made that split explicit). "Play my preview on panel X" copies
  the preview's current values into that player's memory for that piece.
- **Only what differs from the defaults is stored**, keyed by param id. So a piece whose
  defaults improve in a later release improves for everyone who never touched that param,
  and the file stays small.
- **Error handling to defaults, per value, silently correct and logged once**: unknown
  piece id -> entry ignored (kept in the file, so a piece that comes back gets its settings
  back); unknown param id -> ignored; non-finite value -> default; out of range -> clamped
  to the param's range (that is what the sliders do); wrong JSON type -> default. One bad
  value never discards the rest of a piece's memory, and never the rest of the file.
- **Reset** (`reset_params`, and the equivalent for a player) clears that piece's memory in
  that context, so "Reset" means "back to defaults and stay there".
- **Schema**: this is a schema change. Bump to v2 with a v1 -> v2 migration that reads a v1
  file's current `params` into the new memory (nobody loses what they have today), tested.
  Keep the file human-readable. Bounded: memory is per known piece id, so it cannot grow
  without limit; cap the number of unknown-piece entries kept (say 64) so a typo loop or a
  hostile client cannot grow the file for ever.

## Deliverables

- The memory in `state.rs` (+ migration), used by `Engine::set_piece` / `set_param` /
  `set_seed` / `reset_params` and by the device players' equivalents.
- API: no new routes needed; `bootstrap`/`status` should expose enough that the UI shows the
  restored values after a switch (it re-reads state after `set_piece` today - verify, and
  verify a second browser sees them too).
- Tests: switch away and back (preview and player); survives a server restart; each
  error-handling case above; v1 file migrates; Reset forgets; "Play my preview" copies.

## Acceptance

In the Studio: tune `clocks-numerals`, switch to `plasma`, tune it, switch back - both come
back as left, in the design view and on the dashboard's piece selector for a panel, and
still after `docker restart`. A hand-edited state file with garbage values starts a working
server with defaults for exactly the garbage values.

## Log

### Decisions taken as given (orchestrator, recorded here as ordered)

- **What is remembered, per piece id**: the param values and the seed. Not the pipeline
  `settings` (levels, dither, limiter) - those are about the panel, not the piece - and not
  playback state.
- **Scope**: one memory per *context*. The design view (preview) has one; each device player
  has its own. "Play my preview on panel X" copies the preview's current values into that
  player's memory for that piece.
- **Only what differs from the defaults is stored**, keyed by param id, so a piece whose
  defaults improve in a later release improves for everyone who never touched that param.
- **Error handling to defaults, per value, silently correct and logged once**: unknown piece
  id -> entry ignored but kept in the file; unknown param id -> ignored; non-finite -> default;
  out of range -> clamped to the param's range; wrong JSON type -> default. One bad value never
  discards the rest of a piece's memory, and never the rest of the file.
- **Reset** clears that piece's memory in that context: "back to defaults and stay there".
- **Schema**: bump to v2 with a tested v1 -> v2 migration that reads a v1 file's current
  `params` into the new memory. Human-readable. Bounded: per known piece id, and at most 64
  unknown-piece entries.

### Step 0 - claimed

Branch `card/165-per-piece-settings-memory` off `main` (5525f10). Card to `doing/`.
Read first, as instructed: `CLAUDE.md`, `docs/README.md`, card 106's Log (the state store's
guarantees, the `Store::flush` bug, the keep-the-bad-file rule), `crates/studio/src/{state,
engine,player,api,lib}.rs` and `crates/art/src/piece.rs`.

What the reading settled before any code was written:

- `Engine::set_piece` does `self.params = Params::defaults(def.params)` and
  `Player::configure` does `cfg.params.clear()` on a piece change. Those two lines are the
  bug the owner reported.
- `Params::set` already clamps to `[min, max]` (`crates/art/src/piece.rs`), but it accepts a
  NaN (`f32::clamp` with a NaN operand returns the NaN) and `Params::get` panics on an
  unknown id. So the *validation* has to happen before a value reaches `Params`.
- `StoredPlayer.params` and `StoredPreview.params` are `BTreeMap<String, f32>` with
  `#[serde(default)]` on the struct, so a `null`/string/object in the map fails the whole
  struct's deserialise - which under card 106's rules moves the entire file to
  `state.bad.json`. A per-value error must not do that, which is why the new memory
  deserialises through `serde_json::Value` rather than through `f32`.
