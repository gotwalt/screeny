---
id: 150
title: One vocabulary - a piece is a patch, and "settings" is freed for what the owner means by it
type: build
hardware: no
depends: [198, 162]
owner: worker (Claude Opus 5)
branch: card/150-pieces-become-patches
---

## Goal

The owner, 2026-09-20: "let's do patches and settings, sort of like many audio plugins do.
Metaballs as an example is a patch, and then each patch can have multiple named persisted
settings." Asked how deep the rename goes, he chose **everywhere**: the page, the docs, the
CLI, the Rust types and modules, the API and the state file say *patch*. This card is the
rename only. Card 151 builds the named settings on top of it.

## Context

- About a thousand occurrences of "piece" in about ninety files: `crates/art` (the `Piece`
  trait in `piece.rs`, `pieces/`, `PieceSource`, the CLI's `play <piece>`, README),
  `crates/studio` (`set_piece`, `piece_playing`, `piece_act`, `"piece"` in every player
  body and in `/api/v1/status`, `PieceMemory` and `pieces:` in `state.rs`, the three UI
  scripts, tests, README), `docs/design/` (`generative-art-brief.md`, `studio-vision.md`,
  `deployment.md`), the root `README.md`, and the board.
- **This is not a find-and-replace.** A bulk replace once corrupted a README line in this
  repo. Every hit is looked at. English that is not the noun stays English ("a piece of",
  "in one piece", "timepiece"); quoted owner words in card Logs stay as he said them;
  `docs/board/done/` and `docs/research/` are history and are **not** rewritten - add one
  line to `docs/board/README`-level docs (`docs/README.md`) saying that cards before 150
  say "piece" for what is now a patch.
- **The word "settings" has to be freed in the same change**, or card 151 lands on a name
  that already means something else. Today `screeny_art::pipeline::Settings` (the panel
  kind, dither, limiter, `panel_model`, `codec_preview`), `StoredPlayer::settings`,
  `POST /api/v1/set_settings` and `player.settings` in status all mean "how a frame is
  finished for the panel". That becomes **`Output`** / `output` / `set_output` everywhere
  (decided by the orchestrator; the page never showed the word - its sections are "Panel
  model", "Limiter", "View" - so nothing the owner sees changes). After this card the word
  "settings" appears nowhere in code or API except where it is about to mean a patch's
  named settings, the device's own `crates/settings` / `POST /api/v1/settings` (the
  firmware's, **not ours to touch**), and English prose.
- **Nothing that works today may stop working:**
  - *State file.* Schema v3 -> **v4**, by the existing migration pattern in
    `crates/studio/src/state.rs` (backup first, `repaired`/migration note, tests that load
    a v1, v2 and v3 file). Workbench's live `/data/state.json` is a v3 file with the
    owner's tuned parameters in it: write a test from a realistic v3 fixture (dummy names
    only) that proves every player, every per-piece memory, the panel switch and brightness
    arrive intact.
  - *API.* New names are canonical (`set_patch`, `patch_playing`, `patch_act`, `"patch"`
    in bodies and replies, `set_output`). The **old request names keep working on input**:
    old routes stay as aliases, and request bodies accept `"piece"` and `"settings"` via
    `#[serde(alias)]`. The firmware session's scripts call `POST /api/v1/player/set` with
    `{"device","on"}` and sometimes `"piece"`; `set_panel` is unchanged. Replies use the new
    names only - and the three UI scripts are updated with them in the same commit series,
    so the page and the server never disagree.
  - *CLI.* `screeny-art list|play|pipe|snapshot` take a patch name in the same position;
    ids (`metaballs`, `clocks-dials`, ...) do not change. Help text and README say patch.
  - *Pixels.* No rendered output changes. `crates/art`'s tests pass unedited except for
    names.
- `crates/demos` is a different, older thing (protocol demo content) and keeps its words.
  `firmware/`, `crates/proto`, `crates/receiver`, `crates/sim`, `crates/probe`,
  `crates/device-api`, `crates/settings`, `crates/provision` and the protocol spec are
  **out of scope and must not be touched**; if one of them says "piece", leave it and list
  it in the report.
- Card 170's product model stands: one panel, one picture. Card 198: two screens,
  `common.js` + `picture.js` + `panel.js`; `tests/ui.rs` cross-checks ids.

## Deliverables

- The rename, in reviewable commits by area (art types and modules; art CLI and README;
  studio server + state migration; studio UI; studio tests; design docs and root README;
  `CLAUDE.md`'s one mention if any - wording only).
- `git grep -i -w piece` and `git grep -i pieces` over `crates/art`, `crates/studio`,
  `docs/design`, `README.md`: every remaining hit listed in the Log with why it stays.
- The v3 -> v4 migration with its tests; the API aliases with a test each.
- `crates/studio/README.md`'s API table in the new names, with a short "old names still
  accepted" note.

## Acceptance

`cargo test --release --no-fail-fast` and clippy clean; a copy of a v3 state file loads
into the new build with nothing lost; `curl -d '{"piece":"metaballs"}' .../api/v1/set_piece`
still works and `.../set_patch` with `{"patch":...}` does too; the page says Patch.

## Log

### Area 1 - `crates/art`, the types and the modules (2026-09-20)

341 "piece" hits and 66 "settings" hits in `crates/art` on `main`, every one read.
`src/piece.rs` -> `src/patch.rs` and `src/pieces/` -> `src/patches/` with `git mv`, so
history follows. `Piece` -> `Patch`, `PieceDef` -> `PatchDef`, `ShaderPiece` ->
`ShaderPatch`, `piece::` -> `patch::`, `pieces::` -> `patches::`, and the prose with
them. Patch ids are untouched: `metaballs`, `clocks-dials`, `lattice`, ... all as they
were. The CLI's own words changed too (`<patch>`, `which patch?`, `no patch called
'{id}'`, `screeny-art: patch=... apl=... (patch ...%)`), because the binary lives in
this crate and has to compile with it; the crate's README is the next commit.

Three decisions this area forced, none of them in the card:

- **`pipeline::Settings` -> `Output` collided with `pipeline::Output`**, the struct
  `process` returns (wire + preview + stats + measured). That one is now
  `pipeline::Processed`, with a line saying what it used to be called. There is still a
  `crate::output::Output` *trait* (the frame sink), so the two files that import both -
  `src/bin/screeny-art.rs` and `tests/sender.rs` - spell the settings type
  `pipeline::Output` and say why in a comment.
- **`LimiterSettings` -> `LimiterConfig`.** The card wants the word "settings" gone from
  the code, and this is the only other place in `crates/art` that had it. Type name only:
  it is serialised as the `limiter` key of the block above, which has not moved.
- **The WGSL entry point could not be called `patch`: it is a reserved keyword in WGSL**
  (naga refuses the module: "name `patch` is a reserved keyword"). Caught by rendering
  `lattice`, not by the tests, which never reach a shader. It is `fn shade(uv)` now, in
  `fragment.wgsl`, `lattice.wgsl` and `overland.wgsl`.

No pixels moved. `cargo test -p screeny-art`: 70 pass (63 + 4 + 3). Clippy silent. The
five WGSL files are byte-identical to `main`'s once `piece`->`patch` and
`piece(uv`->`shade(uv` are applied to them, so no shader arithmetic changed; and the
three GPU patches were each rendered through `screeny-art snapshot` to prove the
modules still compile (`lattice` 1277 B lossy, `overland` 832 B exact, `knot` 668 B
exact, all on Metal).

### Area 2 - `crates/art/README.md` (2026-09-20)

51 hits, all read. Prose and paths (`patches/clocks/`, `patches/knot.rs`, "Adding a
patch", "GPU and 3D patches"), `Piece::playing`/`Piece::act` -> `Patch::...`,
`ShaderPiece::boxed`/`with_scene` -> `ShaderPatch::...`, `Settings::dither` ->
`Output::dither`, and the shader template's `fn piece(uv)` -> `fn shade(uv)` to match
the code.

Left as it was, beyond the one word: line 11 still calls `crates/studio` a "Tauri v2
desktop app for designing patches". That has been wrong since card 105 made the studio a
server, and putting it right is not this card's business.

### Area 3 - `crates/studio/src`: the server, the API and the v3 -> v4 migration (2026-09-20)

About 420 "piece" hits and 120 "settings" hits across the nine source files, all read.
Nothing here was replaced blind: a JSON key inside a string literal was masked before
the pass and put back afterwards, because the fixtures in `state.rs` **are** old files
and had to stay spelled the old way.

**Names.** `PieceMemory` -> `PatchMemory`, `PieceInfo` -> `PatchInfo`, `SetPiece` ->
`SetPatch`, `PieceAct` -> `PatchAct`, `FaultPiece` -> `FaultPatch`, `FAULT_PIECES` ->
`FAULT_PATCHES`, `MAX_UNKNOWN_PIECES` -> `MAX_UNKNOWN_PATCHES`, `find_piece`/
`fallback_piece`/`default_piece` -> `..._patch`, `Config::fault_pieces` ->
`fault_patches`, `page::pieces()` -> `page::patches()`. Replies: `StudioState.piece`
-> `patch` and `.settings` -> `output`, `Bootstrap.pieces` -> `patches`,
`PlayerStatus.piece`/`piece_name`/`settings` -> `patch`/`patch_name`/`output`,
`PreviewStatus.piece` -> `patch` on `/api/v1/status`. **Replies carry the new names
only**, as the card asks.

**State file, v3 -> v4.** `SCHEMA_VERSION` is 4. `pieces` -> `patches`, a player's
`piece` -> `patch` and `settings` -> `output`. Every old key is still read:
`#[serde(alias)]` on `StoredPlayer::patch`/`output`, on `LegacyPreview` and on
`Persisted::patches`, plus `load`'s own `raw.get("patches").or(raw.get("pieces"))` -
the memory is lifted out of the raw JSON by hand, so it needs the lookup as well as
the alias. `note_retired_levels` now looks in `output` **or** `settings`, since either
shape may still name card 102's retired `levels`; the sentence still quotes
`settings.levels` (that is what the file being read calls it) and points at
`output.panel`.

Migrating **copies the file aside first** (`back_up`): `state.v3.json`, the same fixed
name-per-version the from-the-future path already used, by `fs::copy` so the state file
itself stays put. Best effort - a directory that cannot be written to is a line on the
dashboard, never a refusal to start - and the `recovered` sentence says where the copy
went.

**A bug the version bump uncovered.** `migrate_to_v3` used to run for any file older
than `SCHEMA_VERSION`. With that now 4, a **v3** file would have gone through it with an
absent `preview` block - i.e. `LegacyPreview::default()`, which is the default patch
with seed 0 - and its `else` branch would have written a memory entry for
`clocks-numerals` that nobody had ever asked for. There is a `migrate` wrapper now that
runs the v1/v2 work only for `was < 3`, and a test
(`migrating_a_v3_file_invents_no_memory`) that fails without it.

**Tests** (in `state.rs`, five new): `a_real_v3_file_migrates_to_v4_without_losing_anything`
loads a realistic v3 fixture - a device, a player with tuned params, `focus`,
brightness 96, `paused`/`speed`, a four-entry memory including one for a patch this
build has never heard of - and checks every field arrives, dummy names only
(`aa11bb` / "the shelf"); `migrating_keeps_a_copy_of_the_file_as_it_was` checks
`state.v3.json` is byte-identical and that `recovered` names it;
`what_is_written_after_the_migration_says_patch_and_output` checks the next save
contains `patches`/`patch`/`output` and none of `piece`/`pieces`/`settings`, and
reloads without migrating again; `the_old_names_are_still_read_at_v4` is the alias
path on its own; and the migration-guard test above. Two existing assertions moved
with the schema: `a_real_v2_file_migrates_to_v3` now expects `SCHEMA_VERSION`, and the
round-trip test looks for `"patches"` in the written file.

**API aliases.** Canonical: `POST /set_patch`, `POST /set_output`, `GET /patch_playing`,
`POST /patch_act`. Still routed to the same handlers: `/set_piece`, `/set_settings`,
`/piece_playing`, `/piece_act`. Bodies: `SetPatch.id` takes `patch` and `piece` as
aliases (so the card's `curl -d '{"piece":"metaballs"}' .../set_piece` works, which it
did not before - that route only ever read `id`), `SetOutput.output` takes `settings`,
and `POST /player/set` takes `piece` for `patch` and `settings` for `output`.
`set_panel` is untouched.

**One more collision.** `player.rs` imports the frame-sink trait `output::Output` and
now also needs the settings type `screeny_art::Output`; the trait is imported
`as FrameSink` with a comment, since it is only in scope for method resolution.
`pipeline::Output` (the per-frame result) became `pipeline::Processed` in area 1, and
`tick` returns that.

`cargo test -p screeny-studio --lib`: 62 pass. Clippy silent on the lib and the binary.
The integration tests in `tests/` are the next area and do not build yet.
