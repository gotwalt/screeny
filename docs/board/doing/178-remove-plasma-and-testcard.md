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

### Resumed; the art registry (7 hits looked at)

Merged `main` first (card 177's flock work). The owner's word since the pause:
he does not care about render cost or timing, so the table above stays as the
reasoning that picked the fixture and no before/after suite timing is reported.
What matters is that the load-sensitive studio tests did not get flakier, which
is the three full runs at the end.

`plasma.rs` deleted. `testcard.rs` deleted from the registry and its two card
102 tests moved to `crates/art/tests/dark_ramp.rs`, with only the two grey
strips they read drawn - **on the rows they were on**, because
`Dither::BlueNoise`'s threshold is a function of `(x, y)` and moving a strip
would have silently re-baselined `dither_moves_the_dark_row...`. The hue
sweeps, the colour ramps and the moving line went with the patch: nothing
asserted anything about them.

Seven hits in `crates/art`, one decision each:

1. `patches/mod.rs` `ALL` - both entries removed.
2. `patches/mod.rs` `the_test_card_is_the_same_card_whatever_the_seed` - kept as
   `vesta_is_the_same_face_whatever_the_seed`. Measured before choosing: of the
   three remaining `seeded: false` patches only `vesta` draws identical frames
   on seeds 1 and 999983; both clocks vary mid-dance because the seed picks the
   choreography.
3. `patch.rs`'s `seeded` doc - `vesta` is the example, for the same reason.
4. `tests/rate.rs` `PURELY_A_FUNCTION_OF_T` - two entries dropped; the
   remaining four are unchanged claims.
5. `tests/rate.rs` `checked >= 6` - had to move, and is now
   `checked + skipped == ALL.len()` plus `checked >= 5`. Five CPU patches
   remain, so the old bound would have gone red on a bench with no adapter for
   a reason that has nothing to do with the test.
6. `tests/pinned_time.rs` - "a patch that does not tell the time" is now
   `metaballs` and `flock`, the two that never read `Ctx::now`. **This is the
   only line I touched in that file** (card 184 is also in it, for vesta).
7. `tests/sender.rs` - the second indexed patch is `flock`, whose module header
   states the frame is indexed and exact, which is the claim under test.

`cargo test --release -p screeny-art`: 97 lib + 2 dark_ramp + 4 pinned_time +
2 rate + 3 sender, all green.

### The studio's own source, and the state compatibility (state.rs, player.rs)

**What happens today, checked before changing anything** (the card asks): a
player on a patch this build has not got kept the missing id; `make_core`
quietly played `fallback_patch` so there was a picture at all; the page went on
naming a patch that was not what was playing; and the only record was one line
on stderr at startup. Nothing in `repaired`.

**The repair** is `state::repair_unknown_players`, run inside `load()` after
`migrate()` - after, so a v1/v2 file's player, which `migrate_to_v3` builds out
of the old `preview` block, is looked at too. It puts the player on
`default_patch()` and pushes one sentence into `repaired`. Three deliberate
non-actions, each written where it is done:

- **The memory is not touched.** `clean_memory` already keeps entries for
  unknown patches on purpose (card 165), and this keeps that promise: the owner
  may have spent an evening on `plasma`, and a patch can come back.
- **It is not `fallback_patch`.** That is for a patch that *broke while
  running*, where "not the one that just failed" is the whole point. This is a
  patch that was never here, and the honest answer is what a studio with no
  file at all comes up on.
- **The missing patch's seed, speed and parameters do not come across.** They
  described a different picture. The default patch arrives as the studio last
  left *it*, exactly as switching to it by hand would - which is asserted.

**No schema bump**, and for card 102's and card 161's stated reason: the shape
of the file has not changed and no key has changed meaning. A v5 file with a
player on `plasma` is a valid v5 file; one value in it names something this
build has not got, which is a `repaired`, not a migration. Bumping would force
a backup copy and a "migrated" line on a file that needed neither.

`default_patch()` is `screeny_art::patches::ALL[0]`, which is `clocks-numerals`
- checked, and neither of the two. `fallback_patch`'s ladder was
`["plasma", "metaballs", "clocks-numerals"]` and is now
`["metaballs", "clocks-numerals", "clocks-dials"]`.

Three new tests: a v5 file whose player is on `plasma` (plays the default,
says it once, keeps the memory entry, takes the default patch's own seed and
speed rather than plasma's); two players on removed patches making **two**
sentences, one per panel, with a third panel on a patch that still exists left
alone; and a v1 file whose only player was the design view on `plasma`, which
exercises the ordering against `migrate_to_v3`.

**One consequence worth the orchestrator's eye**, and it is not hypothetical:
the condition is "this build has not got it", which a `--no-default-features`
(CPU-only) studio also meets for `overland`, `lattice` and `knot`. Such a build
now moves a player off a GPU patch and writes that down, where before it played
a fallback and claimed the GPU patch. The tuning survives in the memory either
way; what is lost is "this panel was on overland". I think this is the better
behaviour - it is honest, and card 145 is about exactly that - but it is a
change nobody asked for, so it is in the follow-ups.

36 hits in `crates/studio/src`, each looked at. `metaballs` is the subject of
the memory and named-settings tests, with **`hue`** as the parameter moved
about: every value those tests use (1.0, 1.5, 2.0, 2.5, 3.0, 3.5) is inside
0..360 and none is its default, so a clamp or a "that is already the default"
can never pass by accident - which `size` (0.4..2.5, default 1.0) would have
let happen twice. Where a test needs a *second* patch whose parameters do not
overlap, that is `clocks-dials` and its `dwell`. Three fixtures keep `plasma`
on purpose, because it is now an instance of the thing they are about: the
memory round-trip, `LIVE_V2` and `LIVE_V3` each carry an entry for a patch this
build has not got, and each asserts it survives.

### The studio's test fixtures (10 files, ~120 hits)

Each hit answered by what its test is about. `metaballs` for "some patch that
is not the default, with a parameter to move" (api, fleet, memory, pacing,
preview, soak); `clocks-dials` wherever a second, non-overlapping patch is
needed; `vesta` in `fleet`'s containment test, because the undisturbed panel
must be on something `fallback_patch` would never choose - before, "the other
player was disturbed" could not be told from "it fell back like the other one".

Two needed thought:

- **`traffic.rs`** (card 164) wants a patch that **holds still**, so that mean
  frame bytes is a fact. `metaballs` at `speed` 0 is the test card's trick on a
  patch that is still here: its picture is a function of `ctx.t * speed`, so
  zero speed is the frame at t = 0 for ever, and it reads no clock. The clock
  patches cannot do this - `Core::tick` passes `local_now()` whether or not
  playback is paused, so a *paused* clock still turns over with the minute.
- **`panel.rs`** wants still **and indexed**, because it compares the device's
  decoded frame with the browser's preview byte for byte. `metaballs` was
  tried and **failed** - which is the test doing its job: a continuous patch
  goes out lossy, and then the meter's preview and the link's encoding are two
  runs of the chooser that need not agree to the byte (codec 0x10, 1173 B).
  `flock` is indexed, exact, seeded and clock-free, so the test says what it
  always said. It says it slightly better now: `indexed_exact > 0` as well as
  `indexed_fallback == 0`, so the line cannot pass by the indexed path never
  being taken at all - which is the trap the metaballs version would have set.

`ui.rs` carried card 151's two named `seeded` examples; they are `metaballs`
(true) and `vesta` (false) now, the second measured rather than assumed.

### Docs, the CLI, and what was left alone

`crates/art/README.md` (the `pipe`/`snapshot`/`play` commands and the "copy
this one" recipe, where `flock/` is the indexed example now),
`crates/studio/README.md`'s annotated `state.json`, `docs/design/deployment.md`
and `docker-compose.portable.yml`'s list of what a driver-free build still has.

**Two prose mentions stay**, and neither is about the patch: `palette.rs`'s
header and section 4 of the brief both say "in the lab a plasma and a
Mandelbrot zoom went out exactly on 98-100% of frames", which is card 002's
measurement on `lab/src/content.rs`'s clip of that name and is still true;
section 6 of the brief lists "plasmas" among the *kinds* of work the panel
suits, which is a genre, not an id.

The CLI, run: `screeny-art list` shows eight patches and neither of these;
`snapshot plasma` and `pipe testcard` both give the ordinary
``no patch called `x`; try `screeny-art list` `` error.

**Out of scope, untouched, as the card says:** `crates/demos` (its own older
test card, protocol demo content), `crates/proto/tests/vectors/` (the `plasma`
wire vectors and their README), `crates/screeny/benches/encode/content.rs`,
`lab/` and `tools/gen-vectors` (the lab clip those vectors were generated
from), `firmware/`, `crates/sim`, `crates/receiver` and the spec.
`docs/board/done/`, `parked/` and `docs/research/` are history and are left as
they were written, as are `docs/ORCHESTRATOR.md` and card 107 in `review/`,
which are the orchestrator's own records.

`crates/art/examples/cost_tmp.rs`, the throwaway that measured the candidates,
is deleted.

### What three full-workspace runs found, and the fixture changing again

The per-crate runs were all green. The **first full-workspace run was not**,
and it was worth more than everything before it:

- `studio/soak` and `studio/moved` each lost their player to the **5 s render
  watchdog**. `health` said it plainly: `stalls: 1`, `abandoned: 1`,
  ``last_error: "`metaballs` has not produced a frame for 5s"``,
  `refused: ["metaballs"]`, `fell_back_from`. So "a panel that moved is still
  playing the same thing" failed on the *fallback*, not on the move. In
  `moved` the patch that stalled was `clocks-dials`, the second fixture I had
  picked.
- `studio/api`'s two rate tests (`a_watching_browser_gets_the_full_rate`,
  `the_socket_delivers_frames`) got 17 frames in a second against a bound of
  18. Neither test names a patch - both run on the **default**,
  `clocks-numerals` - so neither is mine; they are the bench being
  oversubscribed by three sequential release runs beside another session's
  build. Worth reporting, not worth changing.
- Run 2 failed `screeny/loopback`, which card 156 already lists as a test that
  fails once on a busy machine.

The watchdog ones **are** mine, and the card's instruction is explicit: do not
make the load-sensitive tests slower or flakier. So the candidates were timed
again, on an idle bench, post-merge (one tick = render + `Pipeline::process`):

| flock | clocks-numerals | clocks-dials | vesta | metaballs |
|---|---|---|---|---|
| **0.415 ms** | 0.661 ms | 1.000 ms | 1.151 ms | 1.628 ms |

**`flock` is the cheapest patch left, for the same reason `plasma` was cheap:
it is indexed and exact**, so the encoder takes the cheap path instead of
running the lossy chooser over a continuous frame. That is the half of
"cheap" the card's framing did not name - most of a tick is the pipeline, not
the patch - and it is why a continuous-tone fixture costs four times an
indexed one however simple its maths.

So the load-sensitive files - `soak`, `moved`, `pacing`, `preview`, and
`panel`, which was already there for the indexed reason - are on **`flock`**.
`traffic` keeps `metaballs`, because what it needs is a patch it can *freeze*
and only metaballs has a speed that reaches zero (`flock`'s `pace` bottoms out
at 0.15). `api`, `fleet`, `memory` and `ui` keep `metaballs`: none is
load-sensitive, the slowest finishes in a second and a half, and it is worth
having a continuous-tone patch among the fixtures.

In hindsight the single-crate green run was the thing that misled me: the
studio suite alone leaves the machine with cores to spare, and the fixture's
cost only shows when the whole workspace is running. Three full runs was the
right instruction.

### The three runs, as actually run

`cargo test --release --no-fail-fast` at the root, three times, with **another
session's full suite running on the same bench for part of it** - which is the
condition card 156 is about and a harder test than an idle box.

| run | result |
|---|---|
| a | **887 passed, 2 failed** - `screeny/indexed::exactness_holds_over_a_stream` ("all thirty displayed") and `screeny/loopback::the_reported_rate_is_the_same_at_any_stream_length` ("10 fps: sender says 9.953, receiver says 9.882") |
| b | **916 passed, 0 failed** |
| c | killed by a SIGTERM at 710 tests (exit 143) through no fault of the suite - another session shares this shell host - so it was **re-run in full**: **916 passed, 0 failed** |

Re-run alone, as the card's rule says: **`indexed` 18 passed, `loopback` 11
passed.** Both are UDP-over-loopback timing tests in `crates/screeny`, a crate
this branch does not touch at all, and `loopback` is already on card 156's
list. Nothing in either names a patch.

**Every test that the fixture change was made for is green in all three**:
`moved` 2, `soak` 1, `pacing` 3, `panel` 5, `preview` 5, `traffic` 4.

`cargo clippy --workspace --all-targets`: **silent**. It was not at first -
two doc comments of mine began a line with a dash and tripped
`doc_lazy_continuation`; parentheses fixed both.

Also checked, because this card changes what `ALL` contains behind two
features: `cargo check -p screeny-studio --no-default-features` and
`cargo check -p screeny-art --no-default-features` both build clean.

### Follow-up work (un-numbered; for the orchestrator to card or drop)

- **A CPU-only studio now moves a player off a GPU patch and writes it down.**
  `repair_unknown_players` asks "has this build got it", which a
  `--no-default-features` studio also answers no to for `overland`, `lattice`
  and `knot`. Before, such a build played a fallback while the page claimed
  the GPU patch; now it is honest, and the file loses "this panel was on
  overland" (the *tuning* survives in the memory either way). I think the new
  behaviour is right and it is what card 145 is about, but nobody asked for
  it, and if it is not wanted the fix is a way to tell "removed" from "not in
  this build" - a list of ids the build knows it was compiled without.
- **`studio/api`'s two rate tests belong on card 156's list.**
  `a_watching_browser_gets_the_full_rate` and `the_socket_delivers_frames`
  assert `n > 18` frames in a second and got 17 on a loaded bench. They are
  not on the known-load-sensitive list and nothing in this card touches them.
- **The 5 s render watchdog is measured against wall clock, not CPU time.** On
  an oversubscribed machine it can refuse a patch that is merely descheduled,
  which costs the player its patch and writes `refused` into its health.
  Making the fixtures cheap dodges it; it does not fix it.
- **`crates/art/README.md`'s "adding a patch" recipe points at `flock/`** for
  the indexed example, which is a directory of six files rather than the one
  file `plasma.rs` was. A small single-file indexed patch would be a kinder
  thing to copy, if one ever wants writing.
