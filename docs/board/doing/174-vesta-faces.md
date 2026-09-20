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
