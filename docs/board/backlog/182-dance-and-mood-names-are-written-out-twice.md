---
id: 182
title: The dance and mood names exist twice, kept in step by a test
type: build
hardware: no
depends: [163]
owner:
branch:
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
