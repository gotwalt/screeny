---
id: 175
title: vesta - the colon is a face's glyph, and a Flip button
type: build
hardware: no
depends: [174]
owner: worker-175
branch: card/175-vesta-colon-and-flip
---

## Goal

Card 174 made vesta's numerals a choice and four of the six faces exactly crisp:
inside a module there are two colours, full ink and true black, and nothing between.
The **colon** is not part of any face. It is still two drawn circles
(`COLON_R = 1.15`, `sample()` in `patches/vesta/mod.rs`), so its edge is
anti-aliased - and that is the whole difference between a 2-colour picture and the
4-colour frame `snapshot --time 04:56` actually reports for a pixel face.

## Context

- Two LEDs of the colon are full and the rest of each dot is a ramp. At the sizes
  involved a "circle" of radius 1.15 LEDs is a 2 x 2 block with soft corners; drawing
  it as a 2 x 2 block of LEDs would look the same across a bedroom and cost two
  palette entries fewer.
- `size` < 1 has to keep working: the colon is scaled with everything else, and below
  1 nothing is crisp anyway. So this is about `size` 1.
- The tidy version is that a face carries a `':'` glyph and the patch asks the face
  for it, which is exactly what `crates/art/src/faces/` was shaped for (glyphs are
  looked up by `char`, and a face that has not got one draws nothing). Terminus,
  Spleen and Dina all have a real colon in their BDFs; Micro Grotesk has none, which
  is why vesta draws its own today. A face without a `':'` would fall back to the
  drawn dots.
- Not urgent and not a bug: 4 colours is far inside `GUARANTEED_PALETTE` and the
  frames are 370-400 bytes of a 1464-byte budget. It is worth doing for the same
  reason the numerals were: the panel is a grid and the picture should land on it.

## Deliverables

- `':'` extracted per face by `tools/art-faces.py` where the font has one, and vesta
  asking the face for it; the drawn dots stay as the fallback.
- `a_pixel_face_is_exactly_crisp_at_size_one` extended past the module boxes to the
  whole panel, for the faces that carry a colon.

## Acceptance

`snapshot vesta --time 04:56 --set font=1` logs `colours=2`.

## Log

### Amendment (the orchestrator, 2026-09-20)

From card 184's report: a rotation can only be watched by waiting for a minute.
So vesta also gains **one action**, `flip`, labelled "Flip", the way the clock
patches offer "Compose another" and "Move on" (`Patch::playing` / `Playing` /
`Patch::act`; the Studio draws an action as a button in "Now playing" and posts
`patch_act`). Pressing it starts a rotation **now**, in whatever `flips` mode is
selected, landing on the current time. In "changed cards only" it must still do
something visible - a full rotation is the honest reading of a button marked
Flip - and what was chosen must be said. It must not disturb the next minute's
own rotation: if it is pressed during one, either ignore it or queue it, and say
which. A pinned-time render must stay byte-identical when the button is never
pressed. It needs a test. The Studio's page should need no change for an action
to appear - check by reading `crates/studio/ui/picture.js`, do not edit
`crates/studio`.

### Step 1: the colon is the face's own, and there is a Flip button

**The colon.** `tools/art-faces.py` now extracts any glyph, not just the ten
numerals, and the **box is still the digits'**: every glyph is sliced out of
the font's own cell with the box the ten numerals share. That one decision is
what makes this work - a colon keeps the position, weight and baseline the font
gave it relative to the digits, so a patch that has placed the digits has
placed the colon too, and the generator refuses (rather than silently clipping)
if a glyph has ink outside that box.

Terminus Bold, Terminus, Spleen and Dina all carry a real `:`. **Micro Grotesk
does not**, and this is worth writing down: rasterising one gives the `.notdef`
box, a rectangle a quarter taller than the digits, and without `fontTools`
(which is not on this machine) there is no honest way to ask a TTF for its
cmap. So its glyph set is **declared** in the face's entry rather than guessed
at. `Vesta` declares the ten numerals too: the pair of dots it draws beside
them is the patch's own, not a glyph.

**Where a colon sits, and the one thing I changed about it.** A font's colon
sits on the baseline, and in a box the digits fill that is near the bottom:
Terminus Bold's dots land 4 LEDs *above* the axle and 10 below it, which on a
split-flap reads as punctuation belonging to the lower card rather than to the
pair. So `Face::middle(ch)` gives the middle of a glyph's ink and vesta lifts
the colon by it. That moves the glyph; it does not redraw it - the font's dots
keep their size, their shape and the gap between them, which is the part that
makes it the same typeface. **`middle` rounds to a whole LED**, which is the
half-LED rule again: a lift of half an LED would put every cell of the colon
across two LEDs and throw away exactly the crispness the face was chosen for.
Spleen is the face that caught it (its raw middle is a half-integer) and the
crispness test is what caught Spleen.

**The Flip button** (the amendment). `Playing` gains one `Action { id: "flip",
label: "Flip" }` and `Patch::act` sets a flag; `render` decides whether to take
it. Two choices the amendment asked me to make and state:

- **It always turns the whole drum**, whatever `flips` says. A button marked
  Flip that did nothing - which is what "changed cards only" would do when no
  card has changed - would be a lie. The two slow modes turn the same eleven
  cards at their own one-card-at-a-time pace, so pressing it also tells you
  what that mode looks like.
- **A press while the board is turning is dropped, not queued.** A real drum
  that is already going round does not go round twice because somebody pressed
  again, and a queue would let a handful of clicks clatter a bedroom for ten
  seconds. The minute always wins: a press is refused while a run is in hand
  and while the minute is turning, so it cannot disturb the minute's own
  rotation.

The flag starts `false` and nothing else reads it, so **a pinned time draws
exactly what it drew before this card** - checked by the test, which renders
three seconds at 10:08:20 and asserts not one LED moves.

**The Studio needs no change.** `crates/studio/ui/picture.js` builds one button
per entry of `playing.actions` and posts `patch_act` with its id (lines 775-781);
it is entirely generic. I read it and edited nothing under `crates/studio`.

**`screeny-art snapshot --act ID[@SECONDS]`**, so a patch whose interesting
behaviour is a *response* can be photographed at all. `snapshot::take_acting`
is the new entry point and `take` calls it with `None`, so nothing else moved.
This is outside my own files (`crates/art/src/snapshot.rs`, `bin/screeny-art.rs`),
additively, and it is what every strip below is rendered with; the clock patches
have offered two actions since card 160 and there has never been a way to
snapshot one.

**Tests.** `a_face_that_carries_a_colon_carries_a_real_one` (two marks, not one
and not a wall; it sits low, as a colon does; inside the box);
`a_pixel_face_is_exactly_crisp_at_size_one` now walks **the whole panel** for a
face that carries a colon rather than stopping at the module boxes;
`the_flip_button_turns_every_drum_and_lands_on_the_time` (in all three modes,
nothing moves before the press, every module moves, the board comes back to the
time it started on); `a_flip_during_a_rotation_is_dropped` (four presses draw
the same film as one); `the_patch_offers_one_action_and_ignores_the_rest`.

**One bug the tests caught, unrelated to either job.** The read-back helper
picked the *first* numeral with nothing missing, and Terminus Bold's `3` is
entirely inside its `8`, so a module showing 8 read as 3. It now breaks the tie
by the most ink of its own: a subset cannot explain the LEDs the superset
lights. It only ever bit a test, never the picture.
