---
id: 178
title: Remove the plasma and testcard patches
type: build
hardware: no
depends: [161]
owner: worker (Claude)
branch: card/178-remove-plasma-testcard
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

### Claimed, and the fixture measured before anything was changed

Branch `card/178-remove-plasma-testcard`, cut from `main` at `6899cca`.

**The fixture question first**, because every other decision hangs off it. The
tests that used `plasma` needed a patch that is cheap, CPU, deterministic,
seeded and continuous-tone. `metaballs` was read (`crates/art/src/patches/metaballs.rs`):
CPU (no `gpu` feature, no wgpu), a pure function of `ctx.t` and its parameters
(no integration, no `ctx.now`), `seeded: true` with every body's radius, path
and phase drawn from `Rng::new(seed)`, and `Frame::supersample` in linear light,
which is continuous tone. All five properties hold.

Cost, measured with a throwaway `crates/art/examples/cost_tmp.rs` (release, 300
frames after 30 warm ones, this bench). "tick" is what a studio player really
pays per frame: `patch.render` plus `Pipeline::process`, which is where the
real encoder runs.

| patch | render | + pipeline | tick | of a 33.3 ms tick |
|---|---|---|---|---|
| clocks-numerals | 2.674 ms | - | 1.768 ms | 5.3% |
| clocks-dials | 2.759 ms | 1.289 ms | 4.048 ms | 12.1% |
| vesta | 2.679 ms | 0.713 ms | 3.392 ms | 10.2% |
| **plasma** | 0.103 ms | 0.195 ms | **0.299 ms** | 0.9% |
| **metaballs** | 0.697 ms | 3.206 ms | **3.902 ms** | 11.7% |
| flock | 0.499 ms | 1.136 ms | 1.635 ms | 4.9% |
| testcard | 0.031 ms | 1.720 ms | 1.751 ms | 5.3% |

So metaballs costs 3.6 ms more a tick than plasma did. That is the one real
cost of this card, and it is worth stating plainly: it is 12% of one core per
player, against 1%, and it is *less* than what `clocks-dials` - which the soak
and the page already play - costs. Nothing in the studio suite runs more than
a handful of players, the waits there are all on conditions with 20-30 s
deadlines, and none of them is a CPU race. Measured both ways at the end of
this card (see the closing entry).

`plasma` was indexed and `metaballs` is not, so the one assertion that leaned on
*indexedness* rather than on cheapness is handled separately (see the panel.rs
entry).

### Paused by the orchestrator

Paused at the orchestrator's request so a `vesta` card could take the slot.
**Nothing of the removal has been written**: the tree compiles, the suite is
untouched, and the only committed change besides this card is the throwaway
`crates/art/examples/cost_tmp.rs`, which produced the table above and is to be
deleted before review.

Where I had got to: the whole scope had been read (every hit of `plasma` and
`testcard` in `crates/art`, `crates/studio` and `docs/`), the fixture had been
chosen and measured, and the next keystroke was going to be deleting
`crates/art/src/patches/plasma.rs` and moving the test card out of the registry.
A baseline `cargo test --release --no-fail-fast` was running for the "suite time
before" number; it was killed at the pause and never finished, so that number
still has to be taken on resume.

**What has been worked out and is not obvious from the card** - all of it
decided by reading, so resuming does not mean re-deciding:

- **The fixture is `metaballs`**, for the properties and the cost above. No
  test-only patch is needed.
- **`default_patch()` is safe**: it is `screeny_art::patches::ALL[0]`, which is
  `clocks-numerals`. Neither of the two.
- **`fallback_patch()` in `crates/studio/src/player.rs` is production code that
  names `plasma` first** (`["plasma", "metaballs", "clocks-numerals"]`). It has
  to lose it.
- **What happens today to a player on a removed patch**: `Player::make_core`
  calls `fallback_patch`, logs `no patch called ...; playing ...` on stderr once
  and sets `health.fell_back_from`. It says **nothing** in `repaired`, and the
  player's *stored* patch id stays as the file had it, so the page names a patch
  that is not what is playing. The card's ask - load, play the default patch,
  say so once in `repaired` - therefore needs a repair at load time in
  `state.rs`, and a test. Memory entries for an unknown patch are already kept
  deliberately (`clean_memory`, `MAX_UNKNOWN_PATCHES`), so nothing there needs
  changing.
- **`seeded: false` after the removal**: `vesta`, `clocks-numerals`,
  `clocks-dials`. Measured: **only `vesta` really ignores its seed** (identical
  frames on seeds 1 and 999983, at t = 2 s and 20 s, with the clock at 0 and at
  a real moment). Both clocks *do* vary with the seed mid-dance - the seed picks
  the choreography - so `vesta` is the replacement example in `patch.rs`'s
  `seeded` doc and the subject of `patches/mod.rs`'s
  `the_test_card_is_the_same_card_whatever_the_seed`.
- **`Core::tick` passes `now: local_now()` whatever `paused` says.** So a
  *paused* clock patch does **not** hold still - it turns over with the minute.
  Any test that needs a still picture (`tests/panel.rs`'s three `hold_still`
  calls, `tests/traffic.rs`'s constant frame size) must use a patch with no
  clock: `metaballs` with `speed = 0` is the direct replacement for the test
  card with `speed = 0` in `traffic.rs`.
- **The one assertion that needs an *indexed* patch** is
  `tests/panel.rs:108`, `indexed_fallback == 0`. With `metaballs` (continuous)
  it becomes vacuous. The remaining indexed patches are `flock` (card 177 is in
  it), `vesta` and the clocks (all clock-driven, so not still). Plan: leave
  panel.rs on `metaballs` and assert what is then true and not vacuous - that
  *neither* indexed counter moves, because metaballs is continuous - and keep
  the real statement where it is already made properly over the same simulator,
  `crates/art/tests/sender.rs::an_indexed_patch_arrives_pixel_exact`.
- **Param renaming is by hand, not by pattern.** `tests/memory.rs` leans on
  plasma's `scale`/`drift`/`cycle`/`bands`/`colours`/`hue` and on `metaballs`
  being the *other* patch. Worked-out mapping: primary `metaballs`
  (`size` for `scale`, `hue` for `hue`, `speed` for the untouched default, and
  `speed`/`spread`/`count`/`samples` for the out-of-range / string / null /
  array garbage values); the *other* patch becomes `clocks-dials`, whose `dwell`
  is a parameter `metaballs` has not got, which is what
  `away["params"].get(...).is_none()` needs.
- **Out of scope and confirmed untouched**: `crates/demos` (its own older test
  card), `crates/proto/tests/vectors/`, `crates/screeny/benches`, `firmware/`,
  `lab/`, `tools/gen-vectors`, and the done/parked cards and research.
  `crates/art/src/palette.rs:14` and `generative-art-brief.md:127,372` say "a
  plasma" about the *lab's* content of 2026, not about the patch, and stay.

Next step on resume: delete `crates/art/src/patches/plasma.rs`, move the test
card's drawing and card 102's two dark-ramp tests into
`crates/art/tests/dark_ramp.rs`, take both out of `ALL`, and commit that before
touching the studio.
