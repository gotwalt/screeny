---
id: 174
title: vesta - the numerals' face is a choice, and the choices include real pixel fonts
type: build
hardware: no
depends: [155]
owner: worker-174
branch: card/174-vesta-faces
---

## Goal

The owner, 2026-09-20, looking at `vesta` on the page: "I don't love the font of the
vestaboard - in particular the 5 has a super long descender that's not symmetrical";
"maybe we look at some pixel fonts for this" (https://www.dafont.com/bitmap.php);
"can font be an option that's configurable"; then two sources:
https://github.com/eliheuer/micro-grotesk and https://github.com/Tecate/bitmap-fonts
("here are some more"); and: "this is all for personal use btw, we will never ever
commercialize this".

`vesta` gets a `font` parameter - a named choice - and a small set of good faces behind it,
most of them pixel fonts that land 1:1 on the LEDs.

## Context

- Today `crates/art/src/patches/vesta/glyphs.rs` is one face: ten numerals as stroked
  paths (a distance field), sampled in two places in `mod.rs` - `module()`'s `on` closure
  and the test helper that reads a module back (`glyphs::distance(..) <= g.half`, the
  second with a 0.4 erosion). The orchestrator has already redrawn its 5, 6 and 9 (commit
  on `main`, "vesta: redraw 5, 6 and 9"); keep that face as the choice called `Vesta`.
- **Why a pixel font is probably right here**: at rest a module is axis-aligned and the
  panel is a grid of discrete LEDs. A bitmap face whose pixels ARE LEDs is perfectly crisp
  at rest, where the stroked face is anti-aliased fuzz at every edge. Mid-flip the card is
  resampled whatever the face is, for six frames; `Frame::supersample` handles a bitmap
  sampled by nearest cell perfectly well.
- **The research is done - use it** (clones are in the session scratchpad, which you can
  read: `/private/tmp/claude-501/-Users-aaron-src-screeny/4b35e91d-00c9-409f-9284-5c17ada4f165/scratchpad/`):
  - `bitmap-fonts/` (the Tecate collection, 455 BDFs) and `fonts/bdf.py`, a 40-line BDF
    parser that returns the ten digits cropped to their common ink box as `#`/`.` rows.
  - A scan of every BDF for digit boxes that fit a 14 x 30 module (numeral box about
    12-13 x 20-22 LEDs): **1:1** - `terminus-font-4.39/ter-u32b.bdf` (13x20, 3-LED strokes -
    the best single fit found), `ter-u32n.bdf` (12x20, 2-LED strokes), `ter-u28b/n` (11x18),
    `spleen/spleen-16x32.bdf` (12x20, squarer, a slashed-less 0 and an open 4); **x2** (fat
    2x2-LED pixels, a deliberately retro look; digits 5-6 x 9-10): gohufont-14, cherry-13,
    Dina 10, ctrld 16, Tamzen8x16, ter-u16n, zevv-peep, kourier and others; **x3** (4x6-7):
    scientifica, creep, lemon, bitocra, spleen-5x8, artwiz.
  - Licences, from the BDF headers and upstream: Terminus Font - SIL OFL 1.1, "Copyright
    (C) 2014 Dimitar Toshkov Zhekov" (newer releases 2020), Reserved Font Name "Terminus
    Font"; Spleen - BSD 2-Clause, "Copyright (c) 2018-2019, Frederic Cambus". For every
    other face you pick, READ its BDF `COPYRIGHT`/`NOTICE` properties and its upstream
    licence before embedding it, and write both down.
  - `micro-grotesk/` - one 16 KB variable TTF (`fonts/MicroGrotesk[wght].ttf`, wght
    100-900), SIL OFL 1.1, "Copyright 2020 The Micro Grotesk Project Authors", a geometric
    grotesque (no colon glyph; we draw our own colon anyway). Its digits are about 0.82 as
    wide as they are tall, so at a 13-LED width they are only ~16 LEDs tall: offer it at
    true proportions, and decide by LOOKING whether a horizontally condensed full-height
    variant is any good (squashing thins the verticals - it may not be). PIL with FreeType
    is available on this machine for rasterising it offline at several weights.
- **The repo is going to be public** (`CLAUDE.md`). The owner's "personal use" is about his
  own use, but committing a font's data publishes it. So: embed only faces whose licence
  allows redistribution (OFL, BSD, MIT, public domain, WTFPL and the like), each with its
  attribution and licence text or reference in `crates/art/src/patches/vesta/FACES.md`
  (OFL: include the copyright notice and the licence; do not use a Reserved Font Name as
  the name of a modified version - naming the *choice* after the font it reproduces
  unmodified is fine, and say in FACES.md that the bitmaps are unmodified extracts of the
  digits). A face that is "free for personal use" only (much of dafont) is NOT committed;
  note in FACES.md how such a face could be added locally. Do not fetch fonts from dafont.
- **No new runtime dependency.** Bitmap faces are data in the source: generate a Rust
  module (`faces.rs`) from the BDFs / the TTF with a small script kept in
  `tools/vesta-faces.py` (inputs by path argument, so it is reproducible from the two
  upstream repos; the generated file says which commit and file each face came from).
  Outline faces are embedded the same way, as a higher-resolution coverage mask (4-8
  samples per LED is plenty under a 6x6 supersample) - no TTF parser at run time.
- **Geometry that must hold**: the axle is a pixel boundary (row 16) and module centres
  are pixel boundaries (x = 8, 23, 41, 56), so an even-width, even-height bitmap lands 1:1
  centred; an **odd** width (Terminus bold is 13) must be offset half an LED so its pixels
  still land on LEDs - at `size` 1 a pixel face must be EXACTLY crisp: every lit LED at
  full level, every other LED black (test it). The 2-row seam cuts whatever face is
  showing. `size` < 1 and the mid-flip frames resample, which is fine. `weight` applies to
  the stroked face only (hide or ignore it for the others - say which; for Micro Grotesk a
  few weights can be separate choices, or `weight` can pick among pre-rendered masks).
- The default: the owner disliked the stroked face and asked for pixel fonts, so default
  to the best pixel face (expect Terminus bold 32) unless looking at them says otherwise.
- `font` is a named choice (card 163's `choice(..)`), so it is a list on the page and is
  saved in a named setting (card 151) like any parameter.

## Deliverables

- `glyphs.rs` gains a face abstraction (`ink(face, digit, x, y, half) -> bool` or
  similar) used at BOTH sampling sites in `mod.rs`; `faces.rs` (generated) with 5-8 faces
  worth having - a range: the stroked `Vesta`, two or three 1:1 pixel faces, one or two
  chunky x2 faces, Micro Grotesk; `tools/vesta-faces.py`; `FACES.md`.
- Tests: every face has ten distinct, legible digits that fit the module and are told
  apart (the existing `no_two_numerals_are_near_each_other` idea, per face); pixel faces
  are exactly crisp at `size` 1; the seam, the palette bound (32, exact), zero green/blue,
  the pinned-time recipes and the flip tests pass for every face.
- PNGs for the owner in the scratchpad's `vesta-faces/`: one contact sheet per face
  (01:23 / 04:56 / 07:08 / 09:59 settled, scale 12), one sheet with all faces side by side
  at 04:56 (the time in his screenshot), and a mid-flip strip for the default face.
  Read them and say honestly which faces are good and which are not worth keeping.

## Acceptance

The owner picks a face on the page and likes his 5.

## Log

### Amendment 1 (the owner, relayed by the orchestrator, 2026-09-20)

> "re vesta: let's add a blank card for leading zero. it's a thing the real
> thing does."

So: when the hours' tens is 0 the first module shows a **blank card** - no
numeral, the seam and the black card as usual - in 24-hour mode (` 9:05`) and
in 12-hour mode, where that is most of the day. Blank is a real card on that
module's drum and flips to and from its neighbours like any other
(`09:59 -> 10:00` falls from blank to 1; `23:59 -> 00:00` falls from 2 to
blank). With `cascade` on, say where blank sits in that drum's order - a real
hours-tens drum only carries blank, 1, 2. Make it the **default**, with a
toggle (`zero`, "Leading zero", off) to bring the numeral back. It must work
for every face. Tests: the module is empty at 09:05 and shows 1 at 10:05; a
flip to and from blank takes one card's fall; the read-back helper understands
blank. Add a settled 09:05 to the contact sheets.

### Amendment 2 (the owner, relayed by the orchestrator, 2026-09-20)

> "we'll probably want to use our font selections in other patches, so we might
> generalize a bit someday or build in a way that makes that easier."

Build it so that is easy, without building the general thing: the faces live in
a shared module of the art crate (`crates/art/src/faces/`: `mod.rs` with the
small API, `data.rs` generated, `FACES.md` beside them; the script becomes
`tools/art-faces.py`), and `vesta` consumes it. The API is small and not
vesta-shaped: a face has a name, a glyph box and a scale, and answers "is there
ink at (x, y) of glyph `c`" - looked up **by `char`**, so a later card can add
letters without changing callers, and a missing glyph is simply empty (which is
also the blank card). A `FACES` list plus the `NAMES` a `choice(..)` needs, so
a second patch is one line. The half-LED alignment rule belongs in the shared
code. No other patch is ported, no letters are added, no text-layout engine.

### Step 1: the shared faces module, the `font` choice, the blank card

**What I did.** Read card 155, card 163 and `crates/art/src/patches/vesta/`,
then scanned all 335 BDFs in the Tecate clone (455 files, 179 *distinct* digit
sets - the collection is full of forks of the same face) for digit boxes that
fit a 14 x 30 module at x1, x2 or x3, and printed the shortlist as ASCII.

**What I looked at and concluded, on the fonts.** The scan's `1:1` list is
short and the card's research was right: `ter-u32b` (13 x 20, 3-LED strokes),
`ter-u32n` (12 x 20, 2-LED) and `spleen-16x32` (12 x 20, 2-LED) are the only
1:1 candidates worth having. `uw-ttyp0`'s `t0-22` looked promising in the scan
at 9 x 20 but its digits are **oldstyle figures** - different heights, some
with descenders - which is not a clock face; dropped. `ter-u28b/n` (11 x 18) is
Terminus again, smaller and no better; dropped. At x2 the collection collapses:
`Dina_r400-10`, `ctrld-fixed-16r`, `Tamzen8x16r` and `ter-u16n` are all the
same 6 x 9/6 x 10 terminal design, and `gohufont-14b` (7 x 9 -> 14 LEDs) fills
the module edge to edge with no black margin. `zevv-peep` has an odd 6 and 9;
`kourier` has a descender on its 4; `mplus_h12r`, `cherry-13-b` and `creep`
leave the `1` as a bare stem. **Licences settled it**: `cherry`, `haxor`,
`gohufont`, `boxxy` and `sq` carry no COPYRIGHT or NOTICE property in the BDF
and the Tecate clone ships no licence files, so none of them can be embedded in
a repo that is going public. `Dina` states "Copyright 2013 Joergen Ibsen, MIT
license" in the BDF itself, `Terminus` states OFL 1.1, `Spleen` states BSD
2-Clause in its COMMENT block. Those three plus Micro Grotesk (OFL, `OFL.txt`
in its own clone) are what can be shipped, and they happen to be the good ones.

**Micro Grotesk.** Measured with Pillow/FreeType: its widest digit is 0.69 of
the cap height at wght 400 (not 0.82 - the card's figure was from a different
measurement), so at true proportions a 14-LED-wide `4` is only 19 LEDs tall and
fills the module edge to edge. Condensed to 0.80 it fits 12.25 x 22 - full
height, verticals still 2 LEDs - so the condensed full-height variant the card
asked me to judge *is* the better one, and that is what is embedded.

**The code.** Per amendment 2 the faces are `crates/art/src/faces/`:
`mod.rs` (the `Face`/`Ink`/`Bitmap` API, `face(..)`, the half-LED rule and its
tests), `stroked.rs` (`git mv` of vesta's `glyphs.rs` - the house face's paths
and `distance`), `data.rs` (generated). Glyphs are looked up by `char`;
`Face::ink(ch, x, y, weight)` and `Face::core(..)` are the only two entry
points a patch needs, and a glyph a face has not got is empty everywhere -
which is how the blank card is drawn without any face carrying a blank.
`tools/art-faces.py` generates `data.rs` from the two clones and records each
one's commit in the file.

**The blank card** (amendment 1). `BLANK = 10`; `cards(minute, hours24, zero)`
is the four cards a time asks for. Each module now has a **drum**: the hours'
tens carries `[blank, 1, 2]` (or `[0, 1, 2]` with `zero` on) and the other
three carry the ten numerals. That is where I put blank in the cascade order -
before 1 - and it makes `23:59 -> 00:00` one card falling from 2 to blank
rather than eight flipping through 3..9, which is what a real three-card drum
does. A module showing a card its drum has not got (the instant after `zero` is
switched) goes straight to the target. `Playing`'s title shows a space.

**Tests.** `the_blank_card_flips_like_any_other` (blank at 09:59, 1 at 10:00,
2 at 23:59, blank at 00:00, each within one card's fall); the read-back helper
reads a module with nothing lit in it as blank, and now matches against the
chosen face's `core` rather than the stroked distance field, so it works for
every face; `the_seam_cuts_every_module` runs with `zero` on, because a blank
module has no seam to see; the parameter list is re-pinned with `font` first
and `zero` after `hours24`. The shared module has four tests of its own: the
names are the faces' own names, every face has ten distinct numerals that fit
the box, a glyph a face has not got is empty, and a crisp face's cells land on
LED boundaries.

`cargo test -p screeny-art --lib vesta`: 21 passed. `--lib faces`: 8 passed.

### Step 2: looked at all seven candidates, kept six; the crispness proof

**What I looked at.** Rendered one contact sheet per face (01:23 / 04:56 /
07:08 / 09:59 / 09:05, settled, scale 12) and an all-faces sheet at 04:56, then
read every one.

- **Terminus Bold** is the best face in the set by a distance and is the
  **default**: 13 x 20, 3-LED strokes, the largest and most confident thing the
  module will hold. Its 5 is a flat bar over a full bowl - exactly the
  complaint the owner made about the drawn one - its 4 is open-topped, its 7
  has no crossbar, its 1 has a foot and its 0 is slashed, so 0/8 and 1/7 are
  never in doubt across a dark room.
- **Terminus** (regular) is the same design a weight lighter; worth its slot on
  a *night* clock, where less ink is a feature.
- **Spleen** is the second voice: square shoulders, a flat-topped 3, a 4 built
  from right angles and a 9 with a closed tail. Crisp, and not a Terminus.
- **Dina at x2** is the chunky retro one: 2 x 2-LED pixels, 12 x 18, so it
  leaves more black card above and below, which reads well as a flap. Its 1 has
  a full-width foot.
- **Micro Grotesk** is the odd one out and I nearly dropped it: it is an
  outline, so it is soft at every edge (19 colours in a settled frame against a
  crisp face's 4) and its 1 is a bare stem. It stays because it is the only
  face in the list with a **round, unslashed 0** and true circular bowls - a
  different voice, not a worse Terminus - and because a soft face next to five
  hard ones is a real choice. If one has to go, this is the one.

**Rejected, with reasons.** `Dina Bold` at x2 was generated and looked at side
by side with Dina: 4-LED strokes close the counters - the 0 becomes a slot, the
5 and the 6 are hard to tell apart - and it doubles the ink on a clock whose
whole point is few lit pixels. Dropped. `cherry-13-b` was the best-looking
second chunky face but its BDF states no licence (see FACES.md); same for
`gohufont`, `haxor`, `boxxy`, `sq`, `ctrld-font`, `zevv-peep` and `kourier`.
`ter-u28b/n` and `ter-u16n` are Terminus again at other sizes and add nothing.
`uw-ttyp0 t0-22` has oldstyle figures. `zevv-peep`, `kourier`, `mplus_h12r` and
`creep` each fail on a glyph (an odd 6, a descender on the 4, a bare 1).

**The crispness proof.** `a_pixel_face_is_exactly_crisp_at_size_one` renders
sixteen settled numerals per face and collects every distinct colour *inside
the module boxes*: a crisp face gives exactly **two** - full ink and true black
- and nothing between. The non-crisp faces are asserted to give more than four,
so the test cannot pass by measuring nothing. The same thing shows on the wire:
`snapshot --time 04:56` logs `colours=4` for every pixel face and `colours=18`
/ `19` for `Vesta` / `Micro Grotesk`. I also looked at Dina zoomed 2x and every
lit dot is the same brightness.

**Micro Grotesk's geometry, measured** rather than assumed: its widest digit is
0.69 of the cap height at wght 400, so at true proportions a full-height `4` is
14 LEDs wide and fills the module edge to edge with no black margin. Condensed
to 0.80 it is 12.25 x 22 - full height, verticals still 2 LEDs - so the
condensed variant the card told me to judge by looking is the better one and is
what is embedded.

`crates/art/src/faces/FACES.md` written: the table, each face's own
COPYRIGHT/NOTICE as it appears in the file, the full OFL 1.1, BSD 2-Clause and
MIT texts, a note on the Reserved Font Name "Terminus Font" (these are
unmodified extracts, so the choice may carry the name), the faces that were
left out for want of a licence, and how to add a non-redistributable face
locally without committing it. `crates/art/README.md`'s vesta section gains the
face table, the half-LED rule and the blank card.

`cargo test -p screeny-art`: 95 + 4 + 3 passed, 0 failed.
