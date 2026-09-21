---
id: 124
title: Flock - birds that actually bank, and a budget check at the extremes
type: build
hardware: no (the owner judges it on the panel through the Studio)
depends: [123]
owner: worker (opus)
branch: card/124-flock-roll-and-lean
---

## Goal

Two things card 123 found and deliberately left alone.

**1. The flock hardly banks.** Card 123 gave a bird a wing that is a real surface, so a
bird rolling through a turn should flash its underside at the camera - it is drawn a
sixth brighter than the top for exactly that. It almost never happens, because the roll
is tiny. Measured over 100 simulated seconds at seed 7 with 6 birds, the largest roll any
big bird reaches is **7 degrees** at the shipped `calm` 0.90, and **13 degrees** even at
`calm` 0.15 / `wild` 0.9. At 16 LEDs across, 7 degrees of roll is about one LED of
difference in the wing's projection: invisible.

**2. At the top of both `birds` and `size` the frame is two bytes inside the budget.**
`birds 150 size 3.0` with the sky peaks at **1462 of 1464** payload bytes (seed 7,
sampled every 4 s over a minute; before card 123 the same sweep peaked at 1433). Every
frame is still encoded exactly and the test suite's own big-bird case is comfortable
(worst 1042), but that corner has no headroom left and nothing tests it.

## Context

- `crates/art/src/patches/flock/sim.rs:1154`: `roll = atan(lateral / G)`, low-passed with
  a 0.35 s time constant. That is what a real bird does and it is *correct*; the reason
  the number is small is that the turn rates are deliberately gentle (`calm` defaults to
  0.90, cards 168 and 177 tuned the view to be watchable). So this is an **aesthetic**
  question, not a physics bug: does the picture want a bird to lean further into a turn
  than the accelerations say it should? A gain on the roll, or a floor under it during a
  turn, would cost nothing in the flight - `roll` is only ever read by `Bird::frame`,
  which is drawing. Try it and look; the owner judges.
- Do not widen `calm`/`wild` to get it. Card 177 and card 168 both measured the view's
  yaw and pitch rates to keep the picture watchable, and those tests
  (`ten_minutes_of_flight`) will fail if the flight gets wilder.
- Card 123's Log says how to find a banked moment: a throwaway `#[ignore]`d scouting
  test that prints, per frame, the biggest in-frame bird's span, roll, glide and how much
  of its wing plane the camera can see. That, and its turntable rig, are worth rebuilding
  rather than judging a bank from the flock.
- For the budget: `every_frame_goes_out_exactly` in `flock/tests.rs` now flies eight
  configurations. Adding `birds 150, size 3.0` on every backdrop is the obvious next case;
  find out first whether it ever actually goes over 1464 across a full 1800 frames on
  several seeds, and if it does, decide what should give - the encoder has a lossy
  fallback, so this is a quality question rather than a failure.

## Deliverables

- A bank the eye can see at `birds` 6 and `size` 2.5, or evidence written into the Log
  that the honest roll is the right one and the flash is not worth chasing.
- Whatever the budget sweep finds, with the extreme case either added to
  `every_frame_goes_out_exactly` or explained in the Log.
- Pictures for the owner, fixed seed, outside the repo tree.

## Acceptance

The owner sees a bird lean into a turn.

## Log

### Note from the orchestrator (2026-09-21): the owner said yes

Shown card 123's bird and told the flock barely rolls, the owner said: "yeah, let's add some
roll & lean capabilities". So the first deliverable is not in question: build the visible
bank. What he is asking for, in order:

- Birds that visibly roll into their turns at the shipped settings with a small, big flock
  (`birds` ~6, `size` ~2.5) - the wing plane tilting toward the camera, the underside
  flash, the inside wing dropping. An exaggerated, drawn roll (a gain and/or a floor during
  a turn, eased in and out so it never snaps) is the expected answer; the flight itself
  stays as cards 168/177 tuned it.
- "Lean" as well as roll: look at whether the body should also pitch with climbs and dives
  more than the velocity vector alone gives, and whether wings should respond (inside wing
  tucked a little in a hard turn, wings half-folded in a dive, spread and raised when
  braking or climbing). Only what reads at 10-20 LEDs.
- ONE new control is allowed if it earns its place: how far the birds lean into a turn
  (0 = the honest roll, default = what looks right), labelled in card 122's plain style and
  distinct from the existing `bank` ("How far the view leans into a turn" - that one is the
  camera). No other new parameters.
- A second worker is finishing card 123's round two in `bird.rs` and the draw code in
  `mod.rs` (body proportions, the wing root). Own `sim.rs` and the roll/lean quantities and
  how they reach `Bird::frame`; if wing-pose changes are needed in `bird.rs`, keep them to
  the pose inputs (angles), not the proportions, and expect to rebase onto round two.

### 2026-09-21 - claimed, and what today's roll measures

Branch `card/124-flock-roll-and-lean`, cut from `d654629`. Read `CLAUDE.md`,
`docs/README.md`, this card, card 123 (its Log, the scouting test and the
turntable), cards 168, 177 and 122, then `crates/art/src/patches/flock/` in full.

Rebuilt card 123's scouting test as throwaway `#[ignore]`d scaffolding (never
committed; it lives in the scratchpad and is spliced into `tests.rs` for a run and
taken straight back out). It flies the real patch's `Tuning` at seed 7 and prints,
per sample, the biggest in-frame bird's span, roll, glide and presentation.

**Before, at the shipped settings** (`birds` 6, `size` 2.5, `calm` 0.90, `wild`
0.65, 100 s of flight, every bird that is on the panel): roll p50 **0.9 deg**, p95
**5.7**, max **7.6**. For birds drawn 6 LEDs or wider: p50 0.9, p95 5.6, max 7.6.
Exactly what the card says - and in the before pictures (`before/turn-a.png` from
t = 139.6, a sustained left turn with the near bird 16-18 LEDs across, and
`before/turn-b.png` from t = 26.3, a gliding right turn) the birds are level. There
is no turn in them at all: the two nearest birds are flat gull shapes that translate
across the panel.

The scout also prints *presentation* - how much of the wing plane the camera can
see. Through those two turns it is 0.0-0.5 and usually under 0.2: nearly edge-on,
which is why the underside flash card 123 drew never fires.
