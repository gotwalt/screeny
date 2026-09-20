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

### Step 4 - it has to be in the volume (owner, relayed mid-card)

The requirement was already the design - the memory is a field of `Persisted` and goes
through card 106's `Store`, so there is no second file and nothing in `localStorage` - but
it is now *pinned* rather than merely true:

- `the_memory_survives_a_restart_including_a_piece_that_is_not_showing` reads
  `state.json` off the disk between the two processes and asserts the values are in it,
  then starts a **fresh process** on the same `--state-dir` and asks for a piece that was
  **not** the one showing. That second half is the part a "resumes what it was playing"
  test would miss, and it is the part that matters for a deploy.
- `a_memory_round_trips_through_the_file` asserts the written file contains `"pieces"`.
- `crates/studio/README.md` says where the memory lives, why it is that file, and what
  happens to a value this build cannot use.

### Step 5 - the evidence

**Root `cargo test --release --no-fail-fast`: 471 passed, 0 failed, 1 ignored.**
`cargo clippy -p screeny-studio --all-targets`: **no warnings in this crate.** (The
pre-existing `screeny-art` and `screeny-demos` warnings are card 125's and were not
touched; nothing clippy says points at `crates/art/src/piece.rs`.)

New tests: 9 in `state.rs`, 6 in `player.rs`, 2 in `crates/art/src/piece.rs`, 9 in
`crates/studio/tests/memory.rs`.

**Rendered in a real browser** - the Chrome extension was connected. A `screeny-sim` on
`127.0.0.1:50801/50802` (`--no-mdns`), a studio on `127.0.0.1:8899` with `--no-discover`
and a temporary `--state-dir`, both under `timeout`. Two tabs.

1. The design view drew and ran at ~62 fps. Picked **Plasma**, dragged the **Scale** thumb
   from 1.20 to **2.97** - a real drag, not a scripted `input` event - and the picture got
   visibly finer.
2. Clicked **Metaballs**, then **Plasma**. The readout said `2.97` and **the thumb was
   back where it had been left**, which is the card's acceptance by eye.
3. **The second tab** opened on `2.97` already. Doing the away-and-back switch *in the
   second tab* moved the **first** tab's slider to the restored value too, so the state
   push carries it and not just the `set_piece` answer.
4. **The dashboard** drew the panel (`bench`, PLAYING, link up, ~2,800 frames sent, `pal8-lz`,
   exact). Changing its **Piece** to Plasma there gave the player `{"scale": 2.97}` in
   `/api/v1/status` - the value tuned in the browser, on the panel, through the one memory.
5. No console errors in either page. Both tabs closed.

Then the restart, with the real binary rather than a test harness: `SIGTERM`, start again
on the same `--state-dir`. The design view came back on `metaballs` with `count 8`, the
**panel** came back on `plasma` with `scale 2.97`, and asking the design view for
`plasma` - **a piece that was not showing** - gave `2.97`. The file itself:

```jsonc
"pieces": {
  "clocks-numerals": { "seed": 0 },
  "metaballs": { "seed": 0, "params": { "count": 8.0 } },
  "plasma":    { "seed": 0, "params": { "scale": 2.97 } }
}
```

One value per piece, because one value per piece is all that was moved.

**Nothing left running.** The simulator and both studios were started under `timeout` and
stopped by hand; `pgrep` for this worktree's path finds nothing. (Two other worktrees'
processes are up - the firmware session's `espflash monitor` on the serial port and
another worker's `fleet` test binary. Not mine, not touched.)

**No hardware, no LAN.** No serial, no flash, no camera. Every address in every run was an
explicit `127.0.0.1`; `--no-discover` on both studios and `--no-mdns` on the simulator, so
nothing could have reached `screeny-4a00a4` or `workbench.local` even by accident.

### Cards written, not done (reserved range 166-169)

- **166** - the dashboard can change a panel's piece and seed but not its parameters, so
  this card's memory and Reset are only reachable for a panel through the API. Says to read
  card 170 first: if the preview and the player become one engine, this is the design
  view's parameter panel pointed at a panel rather than a second set of controls.
- **167** - `state.repaired` is in `/api/v1/status` and in one startup log line, and the
  dashboard does not draw it. That is the one place somebody looks when a piece "came back
  wrong".

168 and 169 are unused.

### Acceptance, against the card

| the card asked for | where it is |
|---|---|
| the memory in `state.rs` (+ migration) | `PieceMemory` / `Memory` / `SharedMemory`, `remember` / `recall` / `forget_params` / `usable_params`, `migrate_v1_to_v2` |
| used by `set_piece` / `set_param` / `set_seed` / `reset_params` and the players' equivalents | `engine.rs`, `player.rs`; `player/set {reset_params}` for the last one |
| no new routes; `bootstrap`/`status` expose enough for the UI | verified: `set_piece` answers with the restored state, the state push carries it, and a second browser and a late one both get it (`a_second_browser_sees_the_restored_values`, and rendered) |
| switch away and back, preview and player | `switching_away_and_back_restores_the_settings`, `a_panel_restores_a_pieces_settings_too`, `a_panel_comes_back_to_a_piece_as_it_left_it` |
| survives a server restart | `the_memory_survives_a_restart_including_a_piece_that_is_not_showing`, and by hand with the release binary |
| each error-handling case | `a_value_this_build_cannot_use_becomes_the_default_and_the_rest_survive`, `rubbish_in_the_memory_costs_exactly_the_rubbish`, `a_hand_edited_state_file_starts_a_working_server`, `a_remembered_value_this_build_cannot_use_is_corrected` |
| a v1 file migrates | `a_real_v1_file_migrates_without_losing_anything`, `a_v1_merge_prefers_what_the_panel_was_playing`, `a_v1_state_file_comes_up_with_what_it_had` |
| Reset forgets | `reset_means_the_old_value_does_not_come_back`, `reset_makes_a_panel_forget_that_piece` (parameters; the seed is kept - see the push-back above) |
| "Play my preview" copies | no copy step needed after the reversal: `promoting_the_preview_needs_no_copy_step` |
| bounded | per known piece id, unknown entries capped at 64 (`unknown_pieces_are_capped`), `repaired` capped at 16, and the writer is card 106's unchanged one-slot atomic one |
| a v2 binary never destroys a file it cannot read | card 106's four cases still pass unchanged, and a bad *value* now never reaches that path at all |
| root `cargo test --release --no-fail-fast` green; clippy clean | 471 passed, 0 failed, 1 ignored; 0 clippy warnings in `screeny-studio` |

**Not done here, on purpose**: the narrow-window layout (161), preview/player fighting over
a panel (144), piece actions on players (140), a scheduler (104), and the unification
itself (170) - the orchestrator said to keep this small so 170 can build on it.

**Changes outside `crates/studio`, in full**: `ParamSpec::sanitise` and one line in
`Params::set` in `crates/art/src/piece.rs`, plus two unit tests in the same file. Nothing in
`crates/art/src/pieces/**` (card 160's worker owns that), nothing in `crates/screeny`,
nothing in `crates/proto`, nothing in `firmware/`.

### For the orchestrator: the deployed service

This is a **schema change against production data**. The live `/data/state.json` is v1
today, so the upgrade migrates it in place on the first start. The migration is read-once
and write-normally: if it is wrong, the old file is gone, so look before and after.

**Before the upgrade**, take a copy and note two things:

```sh
docker exec screeny-studio cat /data/state.json > /tmp/state-v1-$(date +%s).json
python3 -m json.tool < /tmp/state-v1-*.json | head -40
#   "version": 1
#   players[].piece / .seed / .params      <- what the panel is playing
#   preview.piece  / .seed  / .params      <- what the design view was on
```

**After** `tools/deploy-workbench.sh`:

```sh
curl -s localhost:8787/healthz                         # ok
docker logs screeny-studio 2>&1 | grep 'studio: state'
#   "the state file was schema v1; migrated to v2"     <- expected, once
#   anything else on that prefix is worth reading
docker exec screeny-studio cat /data/state.json | python3 -m json.tool
```

In the new file, check:

1. **`"version": 2`**, and a new top-level **`"pieces"`** object.
2. `pieces[<what the panel was playing>]` holds that piece's **seed** and *only the
   parameters that were off their defaults* - so a piece that was never tuned may have an
   entry with just a seed, and that is right, not a loss.
3. `players[0]` and `preview` still say the same piece, seed and params they said in the v1
   copy. **Nothing in the old file is dropped**; `pieces` is added beside it.
4. `/api/v1/status` -> `state.repaired` is `[]`. A non-empty list is not a fault - it is
   "this value did not fit any more, and here is what I did" - but on a file the studio
   wrote itself it should be empty, so a surprise there is worth reading.
5. `state.bad.json` and `state.v3.json` **do not exist** in `/data`. Either would mean the
   file was not used, and card 106's rule would have kept it.

Then the behaviour, in the browser at `workbench.local:8787`:

- Tune `clocks-numerals` (card 160 gave it a `rest` slider - a parameter that did not exist
  when the v1 file was written, which is the case this card is built for), switch to
  `plasma`, tune that, switch back. Both come back as left, sliders and all.
- `docker restart screeny-studio`. The panel resumes what it was playing (card 106, ~6 s),
  and then switch the design view to a piece it was **not** showing: it comes up tuned.
- One thing to know before you look: the memory is **shared** now. Tuning a piece in the
  browser is tuning it on the panel, because the orchestrator reversed the per-context
  decision. Which piece is showing where is still per context; only how a piece is set is
  one fact.

**Rolling back**: a v1 binary handed a v2 file takes card 106's from-the-future path. It
does **not** parse it: it renames it to `/data/state.v2.json`, says so once, and starts
from defaults - which on that box means discovery adopts `screeny-4a00a4` and plays
`clocks-numerals`. Nothing is destroyed, but the panel stops playing what it was playing
until somebody says otherwise. To undo a rollback: put the v2 image back and
`mv /data/state.v2.json /data/state.json`. (I did not run this against the live service;
it is what `a_state_file_from_the_future_is_kept_not_parsed` pins, unchanged by this card.)

### Orchestrator: merged, deployed, verified on the live service (2026-09-19 22:40 PDT)

Merged cleanly on top of card 160 (whose new `rest` param is the live example of "a param
that did not exist when the file was written"). `cargo test --release -p screeny-art -p
screeny-studio`: 128 passed, 0 failed. Reset keeping the seed: agreed, it is what the button's
label promises.

Live service on workbench: backed up the v1 file first (`~/screeny-backups/state-v1-*.json`
on workbench). After the deploy: log line `the state file was schema v1; migrated to v2`
once, `state.repaired: []`, no `state.bad.json`, the panel back on `overland` seed 4242 ~4 s
after the deploy returned. The file on disk stayed v1 until the first change (an unchanged
state is never written) - then `version: 2` with the `pieces` map. Through the API against
the real panel: tuned `plasma` (hue 200, cycle 0.3), switched to `clocks-numerals` and set
`rest` 3, back to `plasma` -> `{cycle: 0.3, hue: 200}` restored; on disk
`pieces: {clocks-numerals: {seed, params: {rest: 3}}, plasma: {...}, overland: {seed}}`.
Then reset both so the owner starts from defaults; the panel was left on `overland`.
