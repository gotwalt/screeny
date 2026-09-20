---
id: 155
title: vesta - a low-light split-flap clock for the night
type: build
hardware: no
depends: [150, 162]
owner: worker (Claude Opus 5)
branch: card/155-vesta
---

## Goal

The owner, 2026-09-20: "let's design a simple low light clock for night time. Defaults to
red numerals, fill only maybe half the pixels, render in 3d as if it were a vestaboard,
where the numerals animate vertically on change. Maybe the patch is called vesta and we
work to explore that visual idea."

A new patch, `vesta`. This card is a **first version to look at and argue with**, not a
finished design: build the idea honestly, expose what is a matter of taste as parameters
he can move on the page, and come back with pictures.

## Context

- **Start after card 150 has merged** (pieces become patches): write it as a patch from
  the first line. Copy the shape of `crates/art/src/patches/clocks/` (a CPU patch:
  `Frame::supersample`, an indexed frame with a small exact palette, `Ctx::now` as the only
  clock, `hours24` and `offset` parameters as the other clocks have them).
- **What a split-flap is, for whoever draws it.** Each character is a stack of flaps on a
  horizontal axle through its middle. At rest you see the top half of the character on the
  upper flap and the bottom half on the lower one, with a thin dark seam between them -
  the seam is the signature, keep it. On a change the upper flap falls forward about the
  axle: behind it the top half of the *next* character is already showing; the falling
  flap carries the old top half on its front until it is edge-on at 90 degrees, and the
  new bottom half on its back as it comes down to cover the old bottom half. A real
  module gets from 3 to 7 by flipping through 4, 5 and 6.
- **"In 3D" at 64 x 32.** A falling flap is a rectangle rotating about a horizontal axis:
  its height is foreshortened by cos(angle), under perspective its free edge comes towards
  the viewer and gets a little wider, and - this is what sells it at this size - its
  **brightness changes with the angle** as it turns through the light, with the flap behind
  it falling into its shadow. All of it is analytic per sample inside
  `Frame::supersample`; no GPU scene is needed, and the CPU indexed path is what keeps a
  red-only picture exact on the wire. Numerals should be vector strokes or a supersampled
  glyph, not a 1-bit bitmap, or a foreshortened flap will shimmer.
- **Low light is the brief, and it has a trap.** Card 102 measured the panel's dark end:
  codes under about sRGB 38 are made of few refreshes and large areas of them *sparkle*
  faintly, which in a dark bedroom is exactly what one would see. So dim is better reached
  by lighting **few** pixels at a **moderate** level than many at a very low one; by pure
  red (one LED die a pixel, and kind to night vision: keep green and blue at zero unless a
  parameter asks); and by the panel's own brightness control, which costs no depth (card
  066). The flap *bodies* should therefore be black or all but black - do not paint 24
  dark-grey cards, they will shimmer - and the 3D has to read from the numerals' own
  shading, the seam, and the moving flap's edge. The average picture level is on the page's
  meter: aim for a resting APL of a few percent and say what it is.
- **Size: spend the pixels.** The owner first said "fill only maybe half the pixels" and
  then withdrew it the same afternoon: "my half the panel assertion is faulty. We might
  need most of the panel in order to make the flipping operation really look right. And I
  think I'd rather spend the pixels than be artificially constrained." So the flip is what
  sizes the clock: make the modules as large as `HH:MM` allows on 64 x 32 - four modules
  around 13-14 LEDs wide and most of the 32 tall, a narrow colon, a pixel or two of true
  black between modules so each reads as its own flap - because a falling flap only looks
  like one if it has enough rows to foreshorten through (a 10-row half-flap passes through
  10, 9, 7, 5, 3, 1 rows; a 6-row one is a blink). Low light then comes from *what* is lit,
  not how little of the panel: pure red, a moderate numeral level, black flap bodies, thin
  strokes if they read better than heavy ones, and the panel's brightness. Keep `size` as a
  parameter so the modest version can still be looked at, and keep the halftone `fill`
  parameter (off by default) as a cheap way to halve the light without shrinking anything.
- Layout to start from: `HH:MM`, four flap modules and a colon, 24-hour by default like
  the other clocks. No seconds. Whether the colon blinks (no: nothing in a bedroom should
  blink) is a parameter at most.
- A patch cannot set the panel's brightness today; brightness is the panel's. Whether a
  night patch should be able to *ask* for one, or a named setting (card 151) should carry
  one, is a question for the owner - write it under "Open with the owner", do not build it.
- Card 162 makes this designable: `screeny-art snapshot vesta --time 21:12 --out x.png` is
  the settled picture, and `--time 21:12:58 --at 3` (or wherever the flip falls) is a
  mid-flip frame, the same PNG every time. Use them for every before/after.
- The limiter (`output.limiter`, APL cap 0.4) will never bite on this patch; leave it.

## Parameters to expose (starting values are the worker's; the point is that they move)

`hue` (default red, 25-30 deg OKLCH or pure sRGB red - try both and say which looks
cleaner in the snapshot), `light` (numeral level), `size` (how much of the panel the clock
takes; default: as much as fits), `fill` (solid ... halftone), `flip` (seconds a flap takes to
fall), `cascade` (flip straight to the new numeral, or through the ones between, as a real
module does), `tilt` (how far off-axis the viewer is: 0 is flat-on, more shows more of the
falling flap's perspective), `seam` (the dark line's weight), `hours24`, `offset`.

## Deliverables

- `crates/art/src/patches/vesta.rs` (or a `vesta/` module), registered; README entry.
- Tests in the style of the clocks': the right numerals for a given `Ctx::now`; a flip
  starts when the minute turns and ends within `flip` (times `cascade` steps); the palette
  stays within `GUARANTEED_PALETTE` so every frame is exact; the resting picture contains
  no green or blue at the default hue; resting APL under a stated bound; pinned-time
  renders are byte-identical (card 162's test pattern).
- In the card Log: the snapshots that were looked at (settled, three mid-flip angles, the
  halftone variant, two sizes), as file names under the scratchpad plus a one-line
  description of each - and the same PNGs copied somewhere the orchestrator can read them
  to show the owner.
- "Open with the owner": everything that is taste.

## Acceptance

On the panel, at night, by the owner's eye. Before that: the orchestrator looks at the
snapshots and the mid-flip sequence reads as a flap falling, not as a wipe or a squash.

## Log

### 2026-09-20, worker (branch `card/155-vesta`)

**Step 1: built it.** `crates/art/src/patches/vesta/` in three files, registered in
`patches/mod.rs`. The model, in the order it matters:

- **One module is 14 x 30 LEDs**, axle on the panel's own centre line (row 16, a pixel
  boundary), four of them at x-centres 8, 23, 41, 56 with a 1-LED black gap inside each
  pair and a 4-LED colon in the middle: columns 1..15, 16..30, [colon], 34..48, 49..63,
  62 of the 64. A numeral is 12 x 22 LEDs of ink inside that. This is the owner's
  "spend the pixels": a 15-row half-card foreshortens through 15, 14, 12, 9, 5, 1 rows.
- **The falling card** is a rigid card hinged on the axle, and one cosine does three
  jobs: `squash = cos(theta - tilt) / cos(tilt)` is the foreshortening, its *sign* is
  which face is towards us (the card's normal dotted with the view direction is the
  same cosine), and its zero is where the card goes edge-on - which `tilt` moves past
  90 degrees, because a viewer above the board sees the front of a card for longer.
  Inverting `v = -r squash / (1 - r sin(theta) / D)` for `r` is one division, so every
  supersample can ask "how far along the card am I".
- **The lighting** is Lambert on those same two vectors, plus a lit free edge that is
  brightest exactly when the face has nothing left to show, plus the card's shadow cast
  on the plate below. The light sits *below* the eye (6 degrees against tilt's 16) on
  purpose: with the light more frontal than the viewer the shadow runs **ahead** of the
  card down the plate (1.0-2.0 LEDs of lead between 100 and 150 degrees) and tucks back
  under it as it lands. Put the light above the eye and the shadow hides under the card
  and buys nothing. There is a test for the lead.
- **The fall is gravity, not an easing curve.** First attempt was a rod released 12
  degrees past balance: the time integral was right but it is *useless* at 30 fps - half
  the fall happens in the first 30 degrees, so the six frames were 0, 2, 11, 28, 59,
  110, 180 - three frames of nothing and then a slam. Rejected. What ships is the same
  equation with the drum's own speed as the initial condition (`PUSH`, the card leaves
  the pin at a quarter of the speed it lands at): the six frames are **0, 13, 30, 52,
  84, 127, 180**, accelerating the whole way with real motion in every one. Integrated
  once into a 256-entry table and inverted.
- **Colour**: one ramp of one hue plus black, 32 entries, so every frame is exact on
  the wire whatever it draws. The hue is picked in OKLCH (the only way a hue slider
  behaves) and then normalised to a ray with its brightest channel at 1 - and a channel
  under 0.002 linear (sRGB 7, the panel's own floor) is **turned off**, because OKLCH's
  gamut search stops just inside the boundary and leaves a thousandth of green behind
  at red. That is a second die lit at about sRGB 3 in every numeral pixel: invisible as
  colour, visible as card 102's dark-end sparkle. At the default hue the emitted sRGB is
  exactly `(x, 0, 0)`; there is a test over every palette entry.

**Step 2: the cascade drifted.** A card lands between frames, and starting the next one
from `t` rather than from when this one should have ended cost a frame a card - by the
fifth card of `09:59 -> 10:00` the modules were visibly out of step for no reason. Fixed:
each landing hands the clock on at `began + fall`, and `step` retires every card that
has landed since the last frame.

**Step 3: looked at it.** PNGs are under the orchestrator's scratchpad,
`.../scratchpad/vesta/` (not committed):

| file | what | judgement |
|---|---|---|
| `01-settled-2112.png` | settled on 21:12, scale 12 | Reads at a glance. The seam is two black rows on the panel's own centre line and it is the signature it was supposed to be. |
| `02-numerals-contact-sheet.png` | all ten numerals (01:23 / 04:56 / 07:08 / 09:09) | All ten distinct. `1` has a foot and a flag and does not look lonely; `4` is open-topped, `7` has no crossbar, `6` and `9` have spines that leave the bowl, `0` is a stadium against `8`'s pinch. |
| `03-flap-five-angles.png` | 29.7, 61.4, 97.1, 127.4 and 144.3 degrees, `flip=0.6` | **This is the one that matters and it reads.** 30: the old top halves squashed and dimmed, no highlight yet. 61: a lit bar near the module top with the new top half just showing above it. 97: the bar has travelled down to the axle, `10:00` is now readable in the top halves and `09:59` still in the bottom ones. 127: the card is below the axle carrying the new bottom halves at half light, bar at its leading edge, the shadow a dark band between it and the old bottom halves. 144: nearly landed. Not a wipe, not a squash - a card falling about a horizontal axle. |
| `04-first-card-at-30fps.png` | one card at the default `flip`, all seven frames the panel gets | The same read in six frames. Every frame moves. |
| `05-halftone-off-on.png` | `fill` 0 against 0.5 | Halves the light (APL 0.95% -> 0.48%) and keeps the size. The strokes do become a screen texture; that is what it is for. |
| `06-size-full-and-modest.png` | 11:11 at `size` 1 and 0.7 | Full size is the design. 0.7 is there for comparison and for an owner who disagrees. |
| `07-four-modules-mid-flip.png` | 09:59:59 + 4 frames, scale 12 | The `:59 -> :00` moment: four cards in the air, `10:00` arriving above, `09:59` still below. |
| `08-tilt-0-16-34-mid-flip.png` | the same frame at tilt 0, 16, 34 | 0 is flat-on and the card goes edge-on exactly at 90. 16 (the default) puts the edge-on moment at the axle with the most to look at. 34 is a viewer standing over the board; also legible, more theatrical. |
| `09-stroke-weight-16-20-24.png` | `weight` 1.6 / 2.0 / 2.4 at 18:14 | 2.4 closes up `8`'s counters. 1.6 is elegant but starts to break up on the dot rendering. 2.0 is the default. |
| `10-across-the-room.png` | 04:56, 17:09, 14:47, 23:38 at scale 4 | The distance test. Every one reads instantly; `4`, `7`, `9` and `6` are unmistakable. |
| `11-cascade-through-6-7-8-9.png` | the minutes' tens cascading 5 -> 6 -> 7 -> 8 -> 9 -> 0 | The cascade works and the four modules land at different times, which is the point of it. |

**Measured**, at the defaults, from the snapshot's own stats line: resting APL **0.95%**,
peaking at **1.23%** with four modules mid-flip; **460-620 bytes** of 1464, codec
`pal8-lz`, **exact** on every frame looked at (settled, mid-flip, halftone, size 0.6,
hue 40, light 220). 15-25 distinct colours in practice out of a 32-entry palette, so
the fixed-rate rung is never needed.

`cargo test --release -p screeny-art`: 86 lib + 4 sender + 3 pinned-time, all green.
`cargo clippy --workspace --all-targets`: silent.

**Step 4: the recipe was written down against the wrong frame.** Two snapshots that
should have been different came out byte-identical, which looked like a bug and was
not - `flip=0.6 --at 1.5` and `flip=0.2 --at 1.1667` are both exactly 0.8333 of a
fall. Chasing it did turn up a real mistake, though: with `--time 09:59:59` the minute
turns on frame **30**, not 31, because the snapshot's 30 fps grid hits t = 1.0 exactly.
The five-angle strip was therefore a study of 36, 72, 112, 144 and 180 degrees under
five wrong labels. Re-rendered at the right frames, and
`the_snapshot_recipes_in_the_readme_land_where_they_say` now drives a module through
the patch's own `step` and the patch's own clock and checks every row of the README's
table, including which frame the minute turns on - no hand interpolation of the fall
curve anywhere, which is what got it wrong the first time.

| flap at | `--at`, with `--time 09:59:59 --set flip=0.6` | actually |
|---|---|---|
| ~30 | `--at 1.2` | 29.7 |
| ~60 | `--at 1.3333` | 61.4 |
| ~90 | `--at 1.4333` | 97.1 |
| ~120 | `--at 1.5` | 127.4 |
| ~150 | `--at 1.5333` | 144.3 |

At the default `flip` the whole fall is `--at 1.0` to `1.2`, six frames, at 0, 13, 30,
52, 84, 127 and 180 degrees - and `04-first-card-at-30fps.png` is all seven of them.

`12-shadow-runs-ahead.png` was added: 111.6 degrees, where the card is barely below the
axle and its shadow leads it by about 1.1 LEDs down the plate.

## Open with the owner

Everything below is taste, and none of it is settled.

1. **Brightness.** The card asks the question and this patch cannot answer it: a patch
   cannot set the panel's brightness, and for a night clock that is the control that
   matters most - it costs no colour depth (card 066) where `light` does. `light` is
   an sRGB code (default 120, card 102's sparkle floor is 38) and it is the wrong knob
   to be reaching for at 3 a.m. Whether a patch should be able to *ask* for a
   brightness, or whether a named piece (card 151) should carry one, is his call;
   nothing was built.
2. **The numeral level itself**: 120 was chosen in the preview, not in a dark bedroom.
   It may want to be half that.
3. **The lit edge.** `EDGE_BOOST` is 3, so the falling card's edge is about sRGB 217
   over roughly 1.2 x 14 LEDs, for four or five frames a minute. It is the single cue
   that sells the 3D and it is also the brightest thing the patch ever draws. If it is
   too much at night it should come down, or become a parameter.
4. **The shadow** (`SHADOW` 0.22, `PENUMBRA` 0.6 LEDs) and the **light's elevation**
   (6 degrees, against `tilt`'s 16). The light being *below* the eye is what makes the
   shadow visible at all; how much shadow is taste.
5. **The fall's shape**, `flap::PUSH` = 0.12 - how hard the drum throws the card off
   the pin. Lower is more of a hang-then-slam, higher is more of a sweep.
6. **`flip` = 0.2 s a card**, so `:59 -> :00` takes up to 1.0 s on the minutes' tens.
   A real board is quicker and noisier.
7. **Leading zeros.** `09:05` shows a `0` in the hours' tens; a real board would show a
   blank card there, and in 12-hour mode that is most of the day. Easy to add as a
   choice; not built because nothing said to.
8. **The colon** is two 2-LED dots at a fifth and four fifths of the module, at 85% of
   the numeral level, and does not blink by default. `blink` exists because the card
   allowed it; the default is off because nothing in a bedroom should blink.
9. **`1` has a foot.** It is what keeps it from being a lonely stroke in a 14-LED
   module (`06-size-full-and-modest.png`, 11:11), but it is a decision, and a
   grotesque would not normally have one.
10. **The module window clips the falling card.** Perspective widens its free edge by
    about a quarter as it passes edge-on, which would put it into its neighbour across
    a 1-LED gap, so the card is clipped to its own window as a real bezel would clip
    it. The widening is then visible in the numeral on the card rather than in the
    card's silhouette.
11. **Thirteen parameters** is already a long page. `weight` and `pace` are the two
    that could go; `pace` is there because without it a flip is a once-a-minute event
    and the studio would be a bad place to judge one.

## Follow-up

None that needs a card. Everything above is a conversation, not work.
