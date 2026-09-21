---
id: 184
title: vesta - on the minute, every position makes a full rotation
type: build
hardware: no
depends: [174]
owner: worker-184
branch: card/184-vesta-rotation
---

## Goal

The owner, 2026-09-20, with the new faces on the panel: "For vesta, let's do an entire
rotation of every position on minute change. I think the fun of a flipboard is that it
flips."

When the minute turns, **all four modules** run through their whole drum and land on the
new time - the ones whose numeral did not change too. Once a minute the board does what a
flipboard is for.

## Context

- Today (`crates/art/src/patches/vesta/`, cards 155 and 174): only modules whose numeral
  changes flip. With `cascade` on a module passes through the numerals between; without
  it, it falls once. The hours' tens has a three-card drum (blank/0, 1, 2); the others
  count in their own ranges. A card's fall is `flip` = 0.2 s: six frames at 30 fps, under
  gravity, with a small settle. `:59 -> :00` on the minutes' tens is about a second.
- What a real board does, and what to take from it: every module carries the **same**
  drum and a full rotation takes every module the same time, which is why a Vestaboard
  refresh is a wave of identical clatter and not four different little animations. Cards
  fall **fast** - ten or more a second - and the next card is already falling before the
  last has landed: several cards are in the air at once near the axle. Modules are not in
  step: they start a few tens of milliseconds apart and drift, which is most of the charm;
  and they *stop* at different moments because each has a different distance to go - the
  board resolves into the new time module by module.
- So the design questions, to settle by LOOKING at strips and saying what was tried:
  1. **The drum.** One drum for every module - blank, 0-9 (eleven cards; perhaps the colon
     or a few letters later, which is what `crates/art/src/faces/` looks glyphs up by
     `char` for) - so a full rotation is the same length everywhere, against today's
     per-position drums. With one drum, "a full rotation and land on the new numeral" is
     eleven cards plus the distance to the target (or exactly one revolution ending on the
     target - choose, and say why). The hours' tens still shows blank for a leading zero.
  2. **Speed.** Eleven-plus cards at 0.2 s each is over two seconds of every minute, and it
     would look like slow motion. A rotation wants its own, faster card time (try 0.08-0.12
     s a card: 3 frames at 30 fps is the floor at which a fall still reads as a fall, not
     a flicker) and overlapping falls - the next card released before the last lands -
     which `flap.rs`'s single-falling-card model does not do today. Show the six-frame
     single fall against the fast overlapped one. Consider easing the *last* two or three
     cards back to the slow, readable fall so the landing is the satisfying part.
  3. **Stagger.** Per-module start offsets and slightly different card times (a few
     percent, fixed per module - they are mechanical parts, not random per minute), so the
     four are never in lock-step. The order they resolve in is part of the look: left to
     right, or minutes first? Try it.
  4. **How long in all.** A guess at a good total: 1.2-1.8 s from first card to last
     landing. A bedroom clock that clatters for three seconds a minute is too much; half a
     second is not a rotation. Make it a parameter and pick a default by eye.
- This is a **night clock**: a full rotation lights more of the panel, more often, and the
  lit edges of four fast cards are the brightest thing the patch draws (card 155 flagged
  `EDGE_BOOST`). Measure peak and mean APL over the rotation and over a whole minute,
  against today's; consider dimming the lit edge during a fast rotation. It must stay a
  quiet thing in a dark room - the motion is the event, not a flash.
- Make it a named choice, because he may want the old behaviours back at 3 a.m.:
  `flips`: "full rotation" (the **default** now), "through the numerals between" (today's
  cascade on), "changed cards only" (today's cascade off). It replaces the `cascade`
  toggle; an old named setting or state that carries `cascade` must still load (card 151:
  unknown parameters are dropped quietly - check that is what happens and that the result
  is sensible).
- Everything else holds: pinned-time renders are byte-identical (`--time 09:59:58 --at N`
  recipes - update the README's frame table for the new timing, and the test that checks
  it), the palette stays within 32 and exact on every frame INCLUDING with several cards
  in the air (check `bytes=` at the busiest frame), zero green and blue at the default
  hue, every face works (pixel faces resampled mid-fall are fine), the blank card is a
  card like any other, `pace` still compresses a minute for watching it.

## Deliverables

- The rotation in `flap.rs`/`mod.rs`, the `flips` choice, tests (every module ends on the
  right numeral for every minute of a day, in all three modes; a rotation ends within its
  stated time; the modules are not in step; palette/exactness at the busiest frame; pinned
  renders identical).
- Strips in the scratchpad's `vesta-faces/` named `184-...`: a whole rotation at the
  default as a contact strip of every frame (about 45 frames - several rows); the same at
  two other speeds; single slow fall against fast overlapped falls; the last half second
  (the landing) enlarged; one at 23:59 -> 00:00 (the hours' tens landing on blank).
- In the Log: APL over the rotation and over the minute, bytes at the busiest frame, the
  timing chosen and what else was tried; "Open with the owner".

## Acceptance

The owner watches a minute turn on the panel and it is fun.

## Log

### Step 1: the rotation, and the five design questions settled by looking

**The model had to change first.** `flap.rs` draws one falling card and the old
`Module` advanced one card and asked for the next when it landed, which can
never overlap. A module is now a **plan**: the card it started from and a `run`
of `Card { to, at, fall }`, worked out once when the minute turns and only read
after that. Everything the renderer needs at time `t` - the plate, what is
standing above the axle, and every card in the air - is a function of that
plan, which is also what keeps a pinned time byte-identical however many frames
have been drawn. Cards in the air are drawn front to back in release order: a
card let go earlier is further round its swing and nearer the eye, and on the
standing stack it was in front of the one behind it, so release order *is*
depth order. `Pose` carries up to `MAX_AIR` of them with their `Fall` already
computed, so `module()` does its trigonometry once a card a frame instead of
once a sample.

**1. The drum.** One drum on every module: blank, then 0-9, eleven cards. The
hours' tens only swaps its blank for a `0` when `zero` is on. A drum only ever
advances one card, so *"a full rotation ending on the new numeral" is eleven
cards plus the distance* - eleven to twenty-one. I looked for a way to give
every module the same card count and there is not one that does not let a
module skip, which is the one thing a flap cannot do. The card's own Context
says what to do instead and it is right: a shared rate with different distances
is exactly what makes a real board resolve position by position. The counts at
the worst minute (09:59 -> 10:00) are 13, 13, 17, 13.

**2. Speed, and overlap.** A rotation needs its own fast card: `AIR` = 0.1 s,
three frames at 30 fps, the floor the card names. The next card is released a
`period` after the last, not when it lands, so one and a half to two cards are
in the air at once. `spin` is the parameter and it means **what a full
revolution takes**, first card released to last card settled, which is the only
definition that stays honest when one module has six more cards than another;
the period is solved from it.

I rendered every frame of the same rotation at `spin` **0.8, 1.15 and 1.7** and
looked (`184-02-*`):

- **0.8** is flicker, not a drum. Frames +5 to +22 are a jumble - you cannot
  tell you are looking at numerals going past, only that something is
  happening. Rejected.
- **1.7** is a slow counter. Every card is fully readable, which sounds good
  and is not: it reads as a machine counting up to the time rather than a drum
  spinning, and at the worst minute it runs nearly 2.6 s. Rejected - a bedroom
  clock that clatters that long is what the card warned about.
- **1.15** is it. Numerals stream past legibly enough to see that they *are*
  numerals, the module reads as one drum turning, and the whole board is done
  in 1.37 s on an ordinary minute and 1.76 s at 09:59 -> 10:00. Default.

**3. Stagger.** `SKEW` = 0, 35, 75, 110 ms and `RATE` = 1.00, 1.02, 1.05, 1.03,
fixed per module - they are machined parts, not dice. Left to right, so the
wave runs the way the time is read and the minutes' units, the one numeral that
changes every minute, is the last to settle. **This is where I got it wrong
first**: my first rates were 1.00, 0.97, 1.04, 0.99, and the 0.97 on module 1
almost exactly cancelled its 35 ms head start - the two modules landed 4 ms
apart, which is an eighth of a frame, i.e. in lock-step. `the_four_modules_are_
not_in_step` is that bug turned into a test. Rates that all pull the same way
as the skew fix it; the gaps are now 61 ms or more.

**4. How long in all.** 1.37 s on a normal minute, 1.76 s at the worst,
including the settle - the card guessed 1.2-1.8 s and that is where it lands.

**5. The landing.** The last three cards ease from the fast `AIR` back to the
slow `flip`, so the drum arrives at a walk. I rendered the last half second of
an ordinary minute with `EASE` 3 and with `EASE` 0 (`184-06`): without it the
final card is three frames and the drum simply **stops**; with it the last card
takes its full six frames and reads as arriving. Keeping it, and it costs about
0.2 s.

**The night clock.** Measured, at 09:59:59 -> 10:00:00, the worst rotation the
clock draws: resting **1.033%** of the panel, rotation mean **1.211%**, peak
**1.518%**, and over a whole minute **1.037%**. Today's flip peaked at 1.23%,
so the rotation costs about a quarter more at its peak for 1.4 s and four
thousandths of a per cent over the minute. It stays a quiet thing in a dark
room.

The lit edge is damped with speed (`EDGE_DAMP` 0.45 at the fastest, full
brightness by the eased landing). **Honest finding: this is not an APL
decision.** I measured the rotation at damping 1.0, 0.7, 0.45 and 0.3 and the
peak moved 1.595% -> 1.567% -> 1.518% -> 1.498% - six per cent across the whole
range, because the edge is a one-LED line and lines are not area. The reason to
damp it is that a dozen pixels at three times the numeral level, moving, is a
*flash* even when it is not APL; and the reason to damp it **with speed** is
that it hands the full brightness back for the landing, where the eye should be
anyway. One mechanism, two wins.

**On the wire.** Settled: 4 colours, 367 bytes. Through the rotation: 16-25
colours, peaking at **605 bytes of 1464**, `pal8-lz, exact` at every frame from
+1 to +54 - the palette is still one ramp plus black and still 32 entries with
a dozen cards in the air. `the_palette_is_one_ramp_and_black` now walks twelve
moments of a rotation and six parameter sets, including `spin` wound down to
0.7.

**`flips` replaces `cascade`**: "Full rotation" (default), "Through the
numerals between" (`cascade` on), "Changed cards only" (`cascade` off). The two
older modes are left **exactly** as card 155 drew them - no stagger, no
overlap, no easing - so a person who picks one gets what they remember, and the
README's five-angle table still lands where it says. An old setting carrying
`cascade` is refused by `Params::set` and dropped (card 151), leaving the patch
on the new default;  `a_setting_that_still_says_cascade_loads` pins that.

**Tests.** `every_module_lands_on_the_right_card_for_every_minute_of_a_day`
walks all 1440 minutes in all three modes, both `zero` settings and both clock
settings, and checks the run ends on the right card and that a rotation turns
past every card of the drum. Plus: the rotation begins on the minute and
*settles* inside 1.2-1.9 s (the drum turns past the new time on the way round,
so "landed" has to mean "and never moved again"); the four modules are not in
step; the blank is turned past by every module; "changed cards only" still
lands every module together and never touches a module whose card did not
change; mid-rotation there are two numerals and a card between them, and the
module may go dark for up to three frames because **the blank card is passing**
- that is a real card, not a hole. `cargo test -p screeny-art --lib vesta`: 26
passed.

### Step 2: the ordinary minute caught a real bug

Rendering **21:12 -> 21:13** - the minute the owner sees fifty-nine times an
hour - showed only the minutes' units turning and the other three standing
still. The run was being planned when *a module's own card* changed, and on an
ordinary minute three of the four are asked for the same card they are already
showing. That is exactly the thing card 184 is about: "an entire rotation of
every position on minute change ... the ones whose numeral did not change too".

The trigger is now **the minute**, not the card. Every module replans when the
minute turns; a module whose card did not change turns its whole drum - eleven
cards - and comes back to it. The two older modes need no special case, because
`route` gives them an empty run when there is nothing to change, which is the
same as standing still.

`every_position_rotates_even_when_its_card_does_not_change` is that bug as a
test: on 21:12 -> 21:13 every one of the four modules must have moved at least
one LED, the board must read 21:13 at the end, a standing module's planned run
must be exactly `DRUM.len()` cards long and end where it started, and both
older modes must leave it alone.

The restrike (`184-07-an-ordinary-minute.png`) is the picture the card asked
for: `21:12` -> `31:12` -> `42:22` -> `53:23` -> ... -> `10:01` -> `21:02` ->
`21:13`, four drums turning in a left-to-right wave and the whole board settled
in 1.37 s.

### Step 3: the strips, the numbers, and what is open with the owner

**Strips** in the session scratchpad's `vesta-faces/`, panel model on. One
honest line each:

| file | what it shows |
|---|---|
| `184-01-rotation-default.png` | every frame of 09:59:59 -> 10:00:00 at the default, 60 frames. The worst rotation the clock draws; all four drums turning, the minutes' tens last to land. |
| `184-07-an-ordinary-minute.png` | **the one to look at first**: 21:12 -> 21:13, the minute he will see fifty-nine times an hour. `21:12` -> `31:12` -> `42:22` -> ... -> `21:02` -> `21:13`, four drums in a left-to-right wave, settled in 1.37 s. |
| `184-02-spin-fast-0.8.png` | too fast. Frames +5 to +22 are a jumble - you cannot tell you are looking at numerals. |
| `184-02-spin-default-1.15.png` | the default. Numerals stream past legibly enough to read as numerals; the module reads as one drum. |
| `184-02-spin-slow-1.7.png` | too slow. Every card fully readable, which makes it a counter rather than a drum, and it runs nearly 2.6 s at the worst minute. |
| `184-03-one-fall-against-a-rotation.png` | the same seventeen frames twice: "changed cards only" on top (the whole board changes in five frames and is flat), the rotation below. This is the before and after. |
| `184-04-the-landing.png` | the last three quarters of a second of 09:59 -> 10:00 at scale 9. Three modules settle, the minutes' tens turns on alone for a dozen frames - which is a real board resolving position by position, not a bug. |
| `184-06-the-landing-eased-or-not.png` | the ease, top, against `EASE = 0`, bottom. Without it the drum stops dead seven frames earlier; with it the last card takes its full six frames and arrives. |
| `184-05-midnight.png` | 23:59 -> 00:00, the hours' tens landing on the **blank** card, so the board settles on ` 0:00`. |

**The numbers, as measured.**

| | before (card 174) | now |
|---|---|---|
| resting APL | 1.033% | 1.033% |
| peak APL | 1.23% (mid-flip) | **1.518%** (mid-rotation) |
| mean over the event | - | 1.211% |
| mean over a whole minute | 1.033% | **1.037%** |
| settled frame | 4 colours, 367 B | 4 colours, 367 B |
| busiest frame | ~620 B | **605 B of 1464**, 25 colours, `pal8-lz, exact` |
| an ordinary minute | one card, 0.2 s | four drums, **1.37 s** |
| the worst minute | five cards, ~1.1 s | four drums, **1.76 s** |

**The timing chosen**: `spin` 1.15 s for a full revolution (11 cards), which
makes the release period 76 ms - two and a half frames - and puts one and a
half to two cards in the air at once. A spinning card falls in 0.1 s (three
frames); the last three cards of every run ease back to `flip` (0.2 s). Modules
let go at 0, 35, 75 and 110 ms and run at 1.00, 1.02, 1.05 and 1.03 times the
period, left to right.

**What I tried and rejected**, beyond the three speeds and the ease:

- *Equal card counts on every module*, so the board would resolve all at once.
  There is no way to do it without letting a module skip a card, and skipping
  is the one thing a flap cannot do. The card's own Context is right: a shared
  rate with different distances is what makes a real board resolve position by
  position.
- *Deriving the period from the longest module*, so the total would be exactly
  `spin` every minute. Rejected: it would make the drum turn at a different
  speed depending on what minute it was, which no mechanism does.
- *Rates that pull against the skew* (1.00, 0.97, 1.04, 0.99) - my first guess,
  and it put modules 0 and 1 four milliseconds apart, an eighth of a frame.
  Fixed, and pinned by a test.
- *Damping the lit edge to save light.* Measured at 1.0, 0.7, 0.45 and 0.3 the
  peak APL moves six per cent in total, because a one-LED line is not area.
  The damping stays, but for flash, not for APL - and the honest version of
  that claim is in the README.

**Open with the owner.**

- **1.76 s at 09:59 -> 10:00 against 1.37 s on an ordinary minute.** The spread
  is the mechanism being honest - the minutes' tens has six more cards to turn
  - but it means one module clatters on alone for about a dozen frames after
  the rest have settled. It looks right to me and it is what a Vestaboard does.
  If he wants the board to land together, the way to do it is to give each
  module its own period so they all finish at once, and I would rather he saw
  this first.
- **The blank card flashes past every module once a rotation.** That is a real
  card on a real drum and I think it is part of the charm, but it does mean an
  ordinary minute has a frame where a position is dark. Taking it off the
  minutes' drums is a one-line change if he dislikes it.
- **`spin` is on the page** (0.7 to 2.5 s), so 1.15 is a starting point rather
  than a verdict.

**Tests, as run.** `cargo test --release --no-fail-fast` at the root: **918
passed, 0 failed**. No load-sensitive test misfired (cards 156/143 - studio
`moved`/`soak`, screeny `pacing`/`loopback`/`embed`/`traffic`, sim `telemetry`),
so nothing needed re-running alone. `cargo clippy --workspace --all-targets`:
silent.

**Outside my own files**: nothing. `crates/art/src/patches/vesta/mod.rs` and
`flap.rs`, vesta's section of `crates/art/README.md`, and this card.
`crates/art/src/faces/` was not touched - the blank card needed no glyph
lookup, as expected.

**Follow-ups** (un-numbered, for the orchestrator to place): card 175, the
colon glyph, is still open and the rotation makes it *more* attractive, not
less - the colon is the only anti-aliased thing left and it is now the only
thing on the panel that never moves; if the faces ever carry a `':'` the
rotation could blink it or turn it with the rest. Separately, a rotation is a
good excuse for a "test the board" action in the studio ("turn it now" without
waiting for a minute), which would make this much easier to look at on the
real panel.
