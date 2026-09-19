---
id: 017
title: Reorganise into one crates/ workspace (art/ and studio move in)
type: build
hardware: no
depends: [011, 016]
owner: orchestrator
branch:
---

## Goal

One cargo workspace under `crates/`, per `docs/design/studio-vision.md` ("Repository
shape"). Mechanical: no behaviour changes.

## Why it waits for 011 and 016

Both are rewriting files in the crates that move or that art will depend on (016 is
itself creating `crates/receiver` and deleting `spike/`). Moving directories under
them buys merge conflicts for nothing. Do this immediately after both merge, and
before the art session starts card 101, so nothing new is built on the old layout.

## Steps

- `git mv art/screeny-art crates/art`, `git mv art/studio crates/studio`; fold
  `art/README.md` into `crates/art/README.md` + `crates/studio/README.md`; delete
  `art/Cargo.toml`, `art/Cargo.lock`; move `[profile.dev] opt-level = 2` (pieces need
  it) into the root as a per-package profile override for `screeny-art`.
- Root `Cargo.toml`: members `crates/*`; `default-members` = everything except
  `studio` (Tauri needs GUI system libs; keep `cargo test` at the root fast and
  portable); drop the `art` exclude; hoist shared deps into `[workspace.dependencies]`
  where it is free.
- One `Cargo.lock`. Check that unifying dependency versions does not change
  behaviour: root tests (209+), art tests (31), `cargo build -p screeny-studio`.
- Fix every path in docs, READMEs, `.gitignore` (`art/studio/gen/` ->
  `crates/studio/gen/`), `CLAUDE.md`, the generative art brief.
- `firmware/` stays outside the workspace (Xtensa target, `esp` toolchain,
  build-std); after 016 it path-depends on `crates/proto` and `crates/receiver`.
  Verify it still builds; no flash needed (no code change).
- Leave `crates/demos` in place; porting fractal + word clock into art as pieces and
  retiring the crate is a separate card.

## Acceptance

`cargo test` at the root runs everything except the studio; `cargo build -p
screeny-studio` and `cargo run -p screeny-art -- list` work; firmware builds; no
references to `art/` remain outside history and done cards.

## Log
