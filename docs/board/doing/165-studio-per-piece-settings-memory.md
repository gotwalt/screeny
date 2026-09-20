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
- ~~**Scope**: one memory per *context* - the design view (preview) has one, and each device
  player has its own - because "what panel X plays" and "what I am fiddling with" are
  different things (card 106 made that split explicit). "Play my preview on panel X" copies
  the preview's current values into that player's memory for that piece.~~
  **Reversed by the orchestrator, 2026-09-19**, after the owner clarified the product: the
  Studio is attached to one panel almost always and the web page is a window onto what that
  panel is doing, so the preview/player split is being unified in card 170. Therefore:
  **one shared memory for the whole studio**, `pieces: { <piece id>: { params, seed } }` in
  `state.json`. Tuning a piece anywhere updates it; switching to a piece anywhere restores
  from it. "Play my preview on panel X" needs no copy step. Reset clears that piece's entry
  everywhere. A per-context memory would have been built for a distinction that is about to
  disappear.
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
  With the reversal above, the migration **merges** the v1 preview's and every v1 player's
  current params into the one map; where they disagree for the same piece, **the player's
  win**, since that is what the panel was showing. Keep the file human-readable. Bounded:
  memory is per known piece id, so it cannot grow without limit; cap the number of
  unknown-piece entries kept (say 64) so a typo loop or a hostile client cannot grow the
  file for ever.
- **It lives in `state.json` under `SCREENY_STATE_DIR` and nowhere else** (owner, relayed
  2026-09-19): no second file, nothing in the working directory, nothing in the browser's
  `localStorage` as the source of truth. That is the volume the container keeps across an
  image rebuild, which is what makes the memory survive a deploy.

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

### Step 1 - the memory, and every path that must not break it

**`crates/art/src/piece.rs`, additive and small** (the one change outside
`crates/studio`): `ParamSpec::sanitise` - out of range is clamped, and a value that is not
a finite number becomes the piece's default - and `Params::set` goes through it. So no
piece can be handed a NaN by *any* path, which matters because `f32::clamp` returns a NaN
unchanged and `Params::get` hands whatever is stored straight to a piece's arithmetic. Two
unit tests in the same file. Nothing else in `crates/art` was touched; card 160's worker
owns `crates/art/src/pieces/**` and this is not in it.

**`state.rs`** - schema **v2**. `PieceMemory { seed, params }`, `Memory` (piece id ->
`PieceMemory`), and four functions: `remember`, `recall`, `forget_params`, `usable_params`.

- **Only what differs from the defaults is stored.** A value put back to its default is
  removed again, so the entry shrinks as well as grows.
- `recall` **writes its corrections back**, so a file that needed fixing is fixed once
  rather than complained about on every switch. That is what makes the card's "logged once"
  true without keeping a set of things already said.
- **Loading is now one parse into a `serde_json::Value`**, with the memory lifted out and
  cleaned before serde sees it. Deliberate, and the reason is in Step 0: a
  `BTreeMap<String, f32>` refuses a null, a string or an object, and under card 106's rules
  that refusal would move the **whole file** to `state.bad.json`. A bad *value* now costs
  exactly that value. The version peek, the from-the-future path and the keep-the-bad-file
  rule are unchanged.
- Unknown piece entries are **kept** - a piece that comes back in a later release gets its
  settings back - and capped at 64, which is the only direction the map can grow in that
  the studio does not control (it only ever writes entries for pieces in the binary).
- `StoreHealth.repaired` is new: what had to be corrected, capped at 16, on
  `/api/v1/status`. It is not a fault and never a 503 - a value being out of range after a
  piece was re-ranged is exactly what the memory is meant to survive.

**`engine.rs` / `player.rs` / `lib.rs` / `api.rs`**: `set_piece` recalls, `set_param` and
`set_seed` record, `reset_params` forgets; the same on a panel, plus
`player/set {reset_params: true}` (a field on an existing route, not a new route). A
player that falls back after a fault now arrives set up the way that piece was left rather
than with `params.clear()`. The memory is restored **before** the saved piece, so putting
it back is an ordinary switch.

### Step 2 - the tests

9 unit tests in `state.rs`, 6 in `player.rs`, 2 in `crates/art`. Every error case the card
lists is one assertion with the good value in the same entry, because "one bad value never
costs the rest" is the claim. The v1 test is a **hand-written v1 file** with a device, a
player and a preview (card 106's shape), not a round-trip of this build's own structs.

### Step 3 - the orchestrator reversed the scope decision (2026-09-19)

Mid-card, after the owner clarified the product: the Studio is attached to one panel almost
always and the web page is a window onto what that panel is doing, so card 170 will unify
the preview engine and the device player into one. **One shared memory**, not one per
context. Recorded in Decisions above; what it changed here:

- `Persisted.pieces` at the top level of `state.json`, and `SharedMemory` - an
  `Arc<Mutex<Memory>>` handle held by the engine and by every player. `Players` owns the
  canonical handle and lends it to each `Player` it makes, so a player created later (a
  panel added at runtime, a `rekey`) gets the same one.
- The "write it down on the way out" step in `set_piece` **went away**. With one map that
  every change writes to, the memory is always current, and remembering on the way out
  could overwrite a fresher value somebody set on the panel.
- "Play my preview on panel X" lost its copy step, as instructed: what was being previewed
  already *is* what that piece is remembered as.
- The v1 migration became a merge: the preview first, then each player, so **a panel's
  values win** on a piece both were on. One unit test is exactly that conflict.
- `restore_preview` no longer re-applies the saved `preview.params`/`seed` when the memory
  already knows that piece. It would have undone the merge (the preview's values would win
  on the first start after the upgrade, which is the wrong way round). It still applies them
  when the memory has nothing - a hand-edited file - so nothing is lost either way.
- No deadlock: the memory's lock is only ever taken *inside* a `SharedMemory` method, never
  while another of the studio's locks is being acquired. Order is always engine-or-cfg ->
  memory, never the reverse.

**Pushing back on one thing, as instructed.** The card says Reset "clears that piece's
memory", and the reversal repeated it as "clears that piece's entry". I have implemented it
as **clears that piece's parameters and keeps its seed**, everywhere. Clearing the seed as
well would mean that after a Reset, switching away and back rebuilds the piece on whatever
seed the *other* piece happened to be on - a visible, arbitrary change that nobody asked
for by pressing a button labelled "Reset parameters". If the orchestrator wants the literal
reading it is one line: `forget_params` also sets `entry.seed = None`, and two test
assertions move with it.
