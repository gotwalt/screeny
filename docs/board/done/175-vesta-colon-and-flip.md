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

### Step 2: how it looks, and what is open

**Strips** in the session scratchpad's `vesta-faces/`, panel model on. One
honest line each:

| file | what it shows |
|---|---|
| `175-01-the-colon-per-face.png` | **the one to look at.** 10:08 in all six faces. Terminus Bold's colon is two solid 3 x 3 blocks, Terminus' and Spleen's 2 x 2, Dina's a pair of fat 4 x 4 ones - each unmistakably the same hand as its digits. Vesta and Micro Grotesk keep the two soft circles, and next to the others you can see why that was worth changing. |
| `175-02-the-colon-at-three-sizes.png` | `size` 1, 0.8 and 0.55. At 1 it is exact; below it the colon is resampled along with everything else and goes soft, which is what `size` does to the whole clock and is no worse for the colon than for the numerals. |
| `175-03-the-colon-blinking.png` | `blink` on, five frames across a second. It still blinks. The gap it leaves is a little more noticeable than the old circles' because the face's colon is tighter, which I think reads better, not worse. |
| `175-04-flip-on-demand.png` | **Flip**, pressed at +1 with the clock twenty seconds from any minute: settled 10:08, then all four drums turn and land back on 10:08 in about 1.2 s. Nothing but the button moved it. |
| `175-05-flip-in-changed-cards-only.png` | the same press in "changed cards only". One card at a time, no overlap, about 2.4 s - slower and far more legible, which is what that mode means and why the button turning the whole drum there is worth having rather than doing nothing. |

**How it looks, in a sentence each.** The colon was the one thing on the panel
that did not belong to the face; now `10:08` in Terminus Bold is one typeface
all the way across, and the block colon sitting square on the seam looks much
more like a station board than the two little circles did. The Flip button is
the first time this patch can be *played with* rather than waited on.

**The card's acceptance, honestly.** It asked for `colours=2`. It is **3**:
black, the numerals, and the colon - which is deliberately dimmer than the
numerals (`COLON_LEVEL`, card 155) and so is its own level. What the acceptance
was really after is that nothing is anti-aliased any more, and that is now
true: three exact colours and not one ramp pixel, where before there were four
with a ramp round every colon dot. Making it two would mean lighting the
punctuation as brightly as the time, which is the wrong trade on a night clock.

**Open with the owner.**

- **The colon is tighter than it was.** A font colon's dots sit closer together
  than vesta's old ±6 LEDs. I think it looks better - it is punctuation, not a
  third numeral - but it is a visible change to a face he has been looking at,
  and `Vesta` and `Micro Grotesk` still show the old wide pair, so the two
  styles are one click apart on the page if he wants to compare.
- **Flip drops a second press rather than queueing it.** If he finds himself
  wanting to watch two rotations back to back, queueing one press is a small
  change.
- **Flip turns the whole drum in "changed cards only".** That mode's own minute
  moves one card; the button moves eleven. It is the honest reading of a button
  marked Flip, and it doubles as a way to see what that mode looks like, but it
  is a deliberate inconsistency and he should know it is there.

**Tests, as run.** `cargo test --release --no-fail-fast` at the root: **923
passed, 0 failed**. No load-sensitive test (cards 156/143) misfired, so nothing
was re-run alone. `cargo clippy --workspace --all-targets`: silent.

**Outside my own files.** Two, both additive, neither touched by card 178:
`crates/art/src/snapshot.rs` gains `take_acting` (and `take` calls it with
`None`, so every existing caller is unchanged), and `crates/art/src/bin/
screeny-art.rs` gains `--act ID[@SECONDS]`. Without them there is no way to
photograph a patch's response to its own button, which every strip of the Flip
action needed - and the clock patches have offered two actions since card 160
with no way to snapshot either. I read `crates/studio/ui/picture.js` to confirm
the page needs no change and edited nothing under `crates/studio`.

**Follow-ups** (un-numbered, for the orchestrator to place): the faces can now
carry any glyph, so a patch that wants letters is a change to `GLYPHS` in
`tools/art-faces.py` and nothing else - `clocks-numerals` is the obvious second
customer. And `snapshot --act` now makes the clock patches' "Compose another"
and "Play it again" photographable for the first time, which is worth a strip
if anyone revisits them.

### Orchestrator, after the merge (2026-09-20)

Looked at `175-01-the-colon-per-face.png`: in the four pixel faces the colon is now the
face's own - square blocks on the seam, the same hand as the digits - and the two soft
faces keep the drawn dots. Merged `--no-ff`. Root suite: 922 passed, 1 failed - the
Studio's `moved` test, card 156's diagnosed race (this card touches nothing in the Studio);
alone it passes. Clippy silent. Three colours rather than the card's two is right: the
colon is deliberately dimmer than the time.
