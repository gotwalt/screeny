---
id: 182
title: The dance and mood names exist twice, kept in step by a test
type: build
hardware: no
depends: [163]
owner: worker-120
branch: card/120-183-182-studio-small
---

## Goal

Card 163 gave the clock pieces' list-valued parameters their names. Two of the
four lists could be taken from the piece itself:

```rust
const REST_CHOICES: &[&str] = &[RESTS[0].name, RESTS[1].name, …];   // from RESTS
const GRID_CHOICES: &[&str] = &["4 x 2", "6 x 3", "8 x 4"];          // small, and checked
```

The other two could not, because the thing that knows the name only produces
one **with an rng in hand**:

- `dance::dance(which, &mut Rng) -> (&'static str, Vec<Phase>)` - the name
  comes out of the same `match` that builds the phases;
- `ambient::Mood::new(which, &mut Rng).name` - same shape.

So `DANCE_CHOICES` (fourteen) and `MOOD_CHOICES` (nine) are written out in the
`PARAMS` blocks and held in step by
`piece::tests::the_named_stops_are_the_pieces_own_names`, which builds every
dance and every mood and compares. The test makes drift loud, but the names
still exist twice, and a new dance means editing two places or watching a test
fail.

## Context

- `crates/art/src/pieces/clocks/dance.rs`: `dance()`'s `match which % DANCES`.
- `crates/art/src/pieces/clocks/ambient.rs`: `Mood::new`'s `match which % MOODS`.
- `crates/art/src/pieces/clocks/mod.rs` and `dials.rs`: the two written-out
  lists and the `choice(..)` declarations that use them.
- Card 163's worker left this deliberately: splitting the name out of those two
  `match` arms touches the bodies of files another worker was cleaning up at the
  time, and the card's scope was the param declaration lines.

## Deliverables

- `dance::NAMES: [&'static str; DANCES]` and `ambient::MOOD_NAMES: [&'static
  str; MOODS]`, with `dance()` and `Mood::new` reading the name from there
  rather than returning a literal - so there is one list and the `match` arms
  carry only what varies.
- `DANCE_CHOICES` / `MOOD_CHOICES` built from them (`"vary"`, the names,
  `"composed"`; `"wander"`, the names).
- `the_named_stops_are_the_pieces_own_names` keeps its dance/mood halves as a
  guard that the *offsets* are still right, which is the part that stays
  hand-written.

## Acceptance

Adding a dance means adding one name in one place; `screeny-art list` and the
studio pick it up with no other edit. No piece's behaviour changes and the
`dance` parameter's range moves only if `DANCES` does.

## Log

### One list each, and the pixels proved unchanged (worker-120)

`dance::NAMES: [&str; DANCES]` and `ambient::MOOD_NAMES: [&str; MOODS]` are now
the only place either set of names is written.

- `dance::dance` takes `which % DANCES` once, the `match` yields only
  `Vec<Phase>`, and the function returns `(NAMES[which], phases)`. The arms lost
  a tuple wrapper each and read better for it.
- `Mood::new` does the same. To get the name out of eight struct literals I
  added `Mood::STILL` (everything at rest) and `Ripple::ZERO` so the arms use
  `..Mood::STILL` and say only what they change - `rings: none` and
  `open_wave: none` disappear from five of them - and the closing expression
  puts `name: MOOD_NAMES[which]` on. **Field order was preserved for every arm
  that calls `rng`** (`ripples` then `open_wave`), because a struct literal
  evaluates its fields in written order and these draw from the rng.
- `DANCE_CHOICES` and `MOOD_CHOICES` are built by `const fn`s from those arrays:
  `"vary"` + `NAMES` + `"composed"`, and `"wander"` + `MOOD_NAMES`. The lengths
  come out of `DANCES`/`MOODS`, so adding a dance moves the parameter's range
  with it and nothing else has to be touched.
- `piece::tests::the_named_stops_are_the_pieces_own_names` keeps both halves,
  now as a guard on the **offsets** (vary/composed at the ends, wander first)
  and on the ranges the pieces shipped, which is the part that is still written
  by hand. Its comment says so.

**Pixel and rng proof.** A throwaway `crates/art/examples/framehash.rs` (card
125's method: FNV-1a over the raw f32 bits of all 2048 pixels) hashed **600
frames** per case at a fixed wall clock, for **95 cases**: `clocks-numerals` at
seeds 1/7/12345/99 x all 14 `dance` values, `clocks-dials` at the same seeds x
all 9 `mood` values, and the composing path at seeds 3/31/314. The 95 hashes
were stable across two runs before the change and are **byte-identical after
it** - so neither the pixels nor the rng sequence moved. The harness was deleted
and never committed.

`cargo test -p screeny-art` (43 tests) green, `cargo clippy -p screeny-art
--all-targets` silent.

### Orchestrator: merged and deployed (2026-09-20)

Merged to `main` cleanly beside card 195. Root `cargo test --release --no-fail-fast`: 717 passed, 0 failed;
clippy silent. Deployed to workbench with card 195 in one batch (state backed up; untouched). Nine seconds
after the deploy: link up on `overland` seed 4242, device facts read (fw 0.5.0, stack_free 12488, no warning
flags), `sockets` present in `/api/v1/status`. From a real Chrome tab on the deployed page (a background
window, so `document.hidden` was true and - correctly - the page asked for no pictures and its canvas
stayed black): a hand-opened socket asking `fps=30&repeat=false` received 144 frames in 4 s of 6196 bytes
each, ~218 KB/s, the last with 939 of 2048 pixels lit; no console errors. Not judged by anyone yet: the
picture drawn in a *visible* tab after this change - the owner's first glance is that check.
