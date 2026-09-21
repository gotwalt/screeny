---
id: 124
title: Flock - birds that actually bank, and a budget check at the extremes
type: build
hardware: no (the owner judges it on the panel through the Studio)
depends: [123]
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
