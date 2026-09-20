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
