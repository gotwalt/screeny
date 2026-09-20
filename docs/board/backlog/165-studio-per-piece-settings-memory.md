---
id: 165
title: Studio remembers each piece's settings - switch away and back, and they are restored
type: build
hardware: no
depends: [106]
owner:
branch:
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
