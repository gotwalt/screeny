---
id: 178
title: Remove the plasma and testcard patches
type: build
hardware: no
depends: [161]
owner:
branch:
---

## Goal

The owner, 2026-09-20: "let's also kill the plasma and test card patches - they're not
interesting". Neither appears in the Studio's patch list, the CLI's `list`, or the docs as
something to play.

## Context

- `plasma` is more than a patch: it is the **fixture patch** of about ten Studio test files
  (`tests/api.rs`, `memory.rs`, `fleet.rs`, `moved.rs`, `pacing.rs`, `panel.rs`,
  `preview.rs`, `soak.rs`, `ui.rs`, fixtures in `src/state.rs` and `src/player.rs`), of
  `crates/art/tests/sender.rs` and `pinned_time.rs`, and an example in `palette.rs` and the
  READMEs - because it is cheap, CPU, seeded and continuous-tone. The tests need a patch
  with those properties; they do not need *that* one. Pick the replacement deliberately
  (`metaballs` is the obvious candidate: check it is CPU, deterministic and `seeded`) and
  change the fixtures by hand, looking at each (no bulk replace - this repo has been burned).
  Where a test's meaning depended on plasma specifically (a rate test that wants a picture
  that changes every frame; card 164's traffic test wants one that holds still), keep the
  meaning.
- `testcard` carries card 102's dark-ramp acceptance tests
  (`the_dark_ramp_is_a_ramp_on_the_device_and_four_bands_without_it` and its sibling) and is
  named in `tests/ui.rs`, `traffic.rs`, `soak.rs`, `state.rs`, `player.rs`, `patch.rs`
  (the `seeded: false` example). Those tests are about the pipeline and the panel model,
  not about a patch a person plays: keep them, with the test card's drawing moved to
  test-only code (`#[cfg(test)]` or `crates/art/tests/`), out of the registry.
- `crates/demos` has its own, older `testcard` and its words; it is a different thing
  (protocol demo content) and is **not touched**. Nor are `crates/proto/tests/vectors/`
  (its `plasma` rows are wire test vectors, generated long ago by `crates/demos`) or
  `crates/screeny/benches`.
- State files: a player or a memory entry naming a patch that no longer exists must load
  without error - the code already handles "a patch this build has never heard of"
  (card 150's v3 fixture has one); check what a *player* on a removed patch comes up
  playing (the default patch), say it once in `repaired`, and test it. Workbench's live
  state has a memory entry for `plasma`; it is fine for that to sit there inert.
- `default_patch()` in `crates/studio/src/state.rs`: check it is not one of the two.
- Docs: `crates/art/README.md`, `crates/studio/README.md`, `docs/design/deployment.md`,
  `docs/design/generative-art-brief.md` where they use either as an example.

## Acceptance

`screeny-art list` and the page show neither; the full suite and clippy are clean; a state
file whose player was on `plasma` loads and plays the default patch, saying so once.

## Log
