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

### 2026-09-21 - the lean, and the body pitch that was tried and dropped

`Bird::lean`, the roll the bird is **drawn** at: `LEAN_MAX * tanh(gain * roll /
LEAN_MAX)` with `gain = 1 + 5 * lean`, so at the shipped `lean` 1 a six-degree
turn is drawn at about thirty and a hard one (`calm` 0, roll near twenty degrees)
arrives at 54 rather than past it. Eased asymmetrically - 0.22 s going over, 0.45 s
coming back - on top of the roll's own 0.35 s low pass. A plain gain with a
symmetrical ease read like a dial being turned; the asymmetry is what makes it read
as a decision, and the ease at all is what keeps a six-fold gain from turning the
roll's own small movements into a flicker.

**Measured, same 100 s, seed 7, six birds, shipped `calm`/`wild`:** honest roll
p50 0.9 / p95 5.7 / max 7.6 deg (unchanged, and the test asserts it stays under 12);
drawn lean p50 5.8 / p95 30.1 / **max 37.2 deg**. Presentation - how much of the
wing plane the camera can see on the nearest big bird - went from 0.0-0.5 to
0.3-0.7 through the same turns, which is the underside flash card 123 drew finally
having a reason to fire.

In the pose: the **inside** wing of a turn takes 0.30 more fold (so it is drawn in
and swept back) at a 29-degree lean, and the tail fan spreads on the drawn lean
instead of the honest roll. Both are pose inputs; no proportion was touched.

**A body pitch was built and then taken out.** `Bird::pitch` held the bird nose-up
out of its own flight path by where it sat in its speed band - a climbing bird pays
for height in speed, so speed already carries the climb, and it gave 6-7 degrees
nose-up in a climb against 0.5 in a dive. At `AOA` 0.26 the difference in the
picture was a pixel; at 0.50 (17 degrees nose-up in that climb) it was **still** a
pixel. The reason is the view: the camera flies with the flock, so a bird is nearly
always seen from behind or ahead, and from there the body is foreshortened to one
or two LEDs - pitching it rotates something that has no length on the panel. The
lean reads for exactly the opposite reason: it rotates the *wingspan*, which is the
long axis in that same view. Removed rather than kept as a subtle change that only
moves the default picture. See `zoom-climb.png`.

Two tests: `birds_lean_into_their_turns` (at `lean` 1 the drawn bank passes 25 deg
while the honest roll stays under 12; at `lean` 0 the drawn bank *is* the honest
roll to within a degree; and the two never disagree about which way by more than
the easing's lag) and `the_lean_never_touches_the_flight` (24 birds, 60 s, `lean` 0
against `lean` 2: every position, velocity, roll and wingbeat phase identical bit
for bit). The second is the one that matters - it is why `ten_minutes_of_flight`
and the view's promises cannot be moved by this control.

One thing found on the way: `Sim::new` settles the flock with `Tuning::default()`,
so the first moment of any run carries a lean from the warm-up whatever the
parameter says. It decays in well under a second; the tests skip two seconds.

### 2026-09-21 - card 123's round two came in, and the "before" was re-taken

Card 123's round two landed on `main` while this was in flight (body 0.70 -> 0.48 of a
span with size, a five-point spine with a head, constant chord to the wrist with a notch
before the tail, `CAMBER` on the hand wing). Brought it in as a merge, not a rebase; no
conflict - `bird.rs` took both sides cleanly, my pose inputs sit inside round two's
proportions and I touched none of them. The "before" pictures were then re-rendered from
a binary built at the merge commit, so every comparison below is *round two with the
lean* against *round two without it*, not against last week's bird.

### 2026-09-21 - the default picture

55 birds, `size` 1, seed 7, against the same code without the lean: **59, 125, 162 and
146 of 2048 LEDs differ** at t = 12, 20, 28 and 36 s (2.9%, 6.1%, 7.9%, 7.1%). Same
flock, same density, same marks - the sky is untouched and the birds are in exactly the
same places, because the flight is bit-identical; what differs is that a bird in a turn
is now drawn canted, and at two or three LEDs across that is a pixel of the dash moving
up on one side and down on the other. See `compare-default.png`.

### 2026-09-21 - the budget at the far corner

The card's second deliverable, done after the lean because a banked wing shows more
area. `birds 150, size 3.0`, four backdrops, three seeds, 1800 frames each. Frames that
could not be encoded exactly, seeds 7 / 11 / 23, within each sky-light, dusk, horizon,
black:

- at the merge commit, without the lean: 0,0,0,0 / **2,10**,0,0 / **1**,0,0,0
- with it: 0,0,0,0 / **37,94**,0,0 / **7**,0,0,0

Worst encoded size is 1464 of 1464 everywhere in that corner, both before and after.

So **the corner is already over**, and card 123 left it that way: at both controls at
maximum a few frames in a hundred take the encoder's lossy fallback. The lean makes it
commoner - worst case 94 of 1800, 5% - exactly as the card predicted, because a banked
wing presents its surface instead of its edge.

Where the edge is, same seed 11, same runs: **150 birds at `size` 1 or 2, and 110 birds
at `size` 3, are exact for a full minute on every backdrop**; 150 at 2.5 is the first to
slip (51 of 1800 on the dusk sky). APL peaks at 27% and the limiter never moves off
x1.00 anywhere in that grid, black backdrop with a wall of big white birds included, so
panel safety is not the question - this is entropy in the index plane and nothing else.

**What should give: nothing.** `corner-150-3.png` is what that corner looks like - 150
birds each drawn three times life size on a 64x32 panel is a wall of overlapping wings
with no composition left in it. It is not a picture anyone watches; it is where two
sliders end. A frame in fifty being approximated there is the fallback doing its job,
and trading the look of every other picture to buy it back would be the wrong way round.

So it is **not** added to `every_frame_goes_out_exactly`, which would have had to be
weakened to hold it. It has its own test instead,
`the_far_corner_of_both_controls_still_fits`: every frame fits the datagram, the limiter
never pulls the picture down, fewer than one frame in ten is approximated - and it
prints the real numbers so the next person sees them.

### 2026-09-21 - what it looks like, honestly

Pictures, in the order to look at them (all seed 7, `birds` 6, `size` 2.5, default
`calm`/`wild`):

1. `zoom-turn-b.png` - the clearest one. Eight consecutive frames from t = 26.6, one LED
   per pixel blown up, before over after. Two birds gliding through a right turn: level
   dashes before, both canted 33-34 degrees after, leaning the same way.
2. `compare-turn-b.png` and `compare-turn-a.png` - the same two turns as whole panels,
   16 consecutive frames each, before over after.
3. `zoom-turn-a.png` - the sustained left turn at t = 140.3, close up.
4. `compare-default.png` - the 55-bird default at four moments, the picture that must
   not change.
5. `tt-pairs.png` - the model itself on the turntable, level over 32 degrees of lean:
   the planform row and the head-on row. Head-on, level is a shallow gull M; banked it is
   an unmistakable diagonal.
6. `zoom-climb.png` - the body pitch that was dropped.
7. `corner-150-3.png` - the far corner of both controls.

**What still looks wrong:**

- **A banked bird in a glide is close to a straight diagonal stroke.** Gliding wings are
  already nearly straight, and rolling them 34 degrees puts the whole bird on one line -
  which is what a banked gull does look like from behind, but it means the two
  best-leaning moments in this flight are also the two least bird-shaped. Beating birds
  keep more shape through the bank.
- **The lean is only as common as the turns are.** p50 is 5.8 degrees: most of the time
  the flock is flying straight and the birds are level, as they should be, so the bank is
  something you catch rather than something you watch. A lower `calm` makes it constant.
- **The underside flash still hardly shows.** Presentation is up (0.3-0.7 against
  0.0-0.5), so the geometry is right now, but at `UNDERSIDE` 0.16 over six ink levels on
  a lit sky the brighter face often quantises to the same index as the darker one. That
  is a tone question rather than a geometry one - card 128.
- **The inside-wing tuck is hard to see in the flock** and easy to see on the turntable.
  It is doing its job - it stops a banked bird reading as a symmetric cross drawn wonky -
  but nobody will point at it.
- The body pitch is gone, so a bird is still drawn exactly along its velocity vector,
  which is not what a flying thing does. It cannot be fixed by pitching the body at this
  resolution with this camera; if it is worth another go it wants the *wings* to carry
  it, not the spine.
