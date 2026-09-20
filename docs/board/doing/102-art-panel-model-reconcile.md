---
id: 102
title: Reconcile the art system's panel model and colour budget with the measured device
type: build
hardware: no
depends: [100]
owner: worker (card 102)
branch: card/102-art-panel-model
---

## Goal

`crates/art` was built from the first version of the art brief. The brief has since been
rewritten from measurements. Bring the preview and the pieces' working rules up to
date, so the studio shows what the panel shows and pieces stop being stricter than
they need to be.

## Context

`docs/design/generative-art-brief.md` sections 1, 2.1, 2.3 as they are now, against
`crates/art/README.md` "Provisional assumptions". Known gaps:

- **Dark end.** The preview hard-quantises to 64 linear levels (`panel.rs`), with
  32/16-level options for "dimmed by scaling". The device now dithers in time (darkest
  visible about sRGB 6, not 34) and brightness no longer costs depth. The 32/16 options
  are obsolete; the dark end needs a model of the time-averaged result (and perhaps its
  faint sparkle under sRGB ~30).
- **Pieces avoid the darks by rule.** `overland` cuts any palette colour below OKLCH
  L 0.3 to black; the clock pieces start their ramps at L 0.32. With temporal dithering
  "slow fades to black are now usable". Revisit with the owner: some of this is now
  style rather than necessity.
- **Colour budget.** Pieces keep to 32 colours because that was the only exact case.
  `PAL8_LZ` makes up to 256 exact when the index image compresses, and spatial
  coherence, not colour count, is the real cost. `Palette`, the studio's Colours and
  Frame size meters, and the advice in `crates/art/README.md` should say so. Best done after
  101, with the real encoder answering.
- **Frame rate.** The art system assumes 60 fps on the owner's instruction; the brief says 30
  is the design rate and WiFi sets the ceiling. Decide what the sender does when 60
  does not get through, and show drops in the studio once 101 exists.

## Deliverables

Updated `panel.rs` / `pipeline.rs` / studio controls; pieces adjusted where the owner
wants; `crates/art/README.md` assumptions table brought up to date.

## Acceptance

Studio preview of the test card agrees with a camera capture of the panel showing the
same frames (capture by the orchestrator), to the eye, in the dark ramps.

## Log

### Note from card 066 (2026-09-19)

What the device's dimming actually is, in case it saves 102 the reading. The
firmware quantises at full depth (`firmware/src/gamma.rs` is the sRGB EOTF and
brightness is deliberately not in it) and then dims by shortening the
output-enable window: `display::slots_for(b) = (b * 25 + 127) / 255` lit slots
out of `MAX_OE_SLOTS = 25`, linear in duty and so linear in light. All 64 duty
levels exist at every brightness; only the light goes away.

Card 066 put that in `screeny_panel` as `MAX_OE_SLOTS`, `oe_slots(b)` and
`oe_light(b)`, and `Lut::new` now quantises first and scales the emitted light
after. So "dimmed by scaling" is not a thing the panel does at any brightness,
and `crates/art`'s 32/16-level options are not a dim room - they are the old
bug. If 102 wants "what a dim room looks like", it is 64 levels times
`oe_light(brightness)`, not fewer levels. The pre-020 order is still available
as `Lut::value_scaled` if a comparison is wanted.

Also worth knowing for any brightness control in the studio: the control has 25
steps, not 256, and brightness 1..=5 lights zero slots (card 136).

### 2026-09-20 - claimed

Branch `card/102-art-panel-model`, own worktree. Scope as the orchestrator set it:
replace the obsolete 32/16-level model with the device's real time-averaged dark
end, bring the colour-budget advice and meters in line with the real encoder
(card 101), update `crates/art/README.md`'s assumptions. No hardware, no camera;
acceptance is tests pinned against `crates/panel` plus the test card's dark ramp.
Pieces' own look is not to be changed on my taste - the `overland` L 0.3 cut and
the clocks' L 0.32 ramp floor go under "Open with the owner".

### Step 1 - `crates/panel`: `DEVICE`, and the dark end pinned to the firmware

Read `firmware/src/gamma.rs` and `firmware/src/display.rs` (read only). What the
device does: the gamma table is the sRGB EOTF scaled to `63 * 16` (`FRAC_BITS = 4`
fractional bits below a duty level), and `display::quantise_dither` bumps the level
when the remainder beats the phase threshold, with `phase` advancing once per panel
refresh in `main.rs`'s display task and a Bayer 4x4 offset per pixel so the whole
panel does not beat in unison. Over the **full 16-phase cycle** - 16 refreshes,
about 104 ms at the measured 154 Hz, so three 30 fps frames - a held colour averages
exactly `SRGB_TO_Q[v] / 16` levels out of 63.

So the model is `Panel::dithered(6, 16)`, added as `screeny_panel::DEVICE` with
`DITHER_PHASES = 16`. Measured (new tests in `crates/panel/src/model.rs`):

| | `NOMINAL` | `TEMPORAL` | `DEVICE` |
|---|---|---|---|
| duty steps | 63 | 315 | 1008 |
| distinct levels from the 256 sRGB codes | 64 | 195 | 237 |
| codes that come out black | 22 | 6 | 2 |
| darkest lit code | 22 | 6 | 2 |

`device_is_the_firmwares_gamma_table` checks all 256 entries against a copy of
`SRGB_TO_Q` (a fixture, not a second implementation - `firmware/` is a separate
cargo project on the esp toolchain and cannot be depended on). It reproduces the
firmware's own comment exactly: "only sRGB 0 and 1 emit nothing". The brief's
"darkest visible about sRGB 6" is `TEMPORAL`'s first lit code, which is the right
number for a colour on screen for one frame.

`TEMPORAL` is **unchanged** - the encoder scores against it and that is correct:
scoring a codec is a per-frame question and a single frame only gets ~5 of the 16
phases. Its doc now says which question it answers. `DIM`'s "a dim room is half the
levels" is corrected to card 066's answer.

Also measured: the collapse is confined to codes 0..=38 (19 codes move, at most four
to a level). Above 38 the device shows back exactly the code it was sent.

### Step 2 - `crates/art`: quantise and preview against the device

`crates/art/src/panel.rs` is now a reading of `screeny_panel` rather than a second
model. `Settings::levels: u32` becomes `Settings::panel: Panel`, one of `dithered`
(the device) or `bit_planes` (64 levels, no temporal dither - the comparison the
brief asks for). `screeny-art --levels` becomes `--panel dithered|bit-planes`.

Two **deliberate pixel changes**, both in the dark end:

1. A linear frame is quantised to 1008 duty steps, not 63. The test card's dark
   ramp - row 4, sRGB 0..63 stretched across the 64 columns - goes from **4
   distinct levels to 45**. That is the card's acceptance in one number: the ramp
   was four bands and three-quarters black, and it is now a ramp.
2. The dither bias is one **duty step**, not one of 64 levels. Above sRGB 38 a duty
   step is a tenth of an 8-bit code, so ordered dither now rounds away to nothing
   in the midtones - no texture the panel cannot show, and nothing spent on the wire
   compressing noise. Below 38, where up to four codes share a level, a duty step is
   bigger than a code and the dither does the whole job. Measured in
   `dither_lands_only_where_the_panel_is_coarser_than_the_codes`: full-amplitude
   dither moves nothing at sRGB 200 and a whole duty step at sRGB 10.

Hand-over fidelity, measured over 10001 linear values: quantise-then-encode-as-a-code
costs at most **4 duty steps of 1008** (0.4% of full light), at the top of the range
where the 8-bit codes are coarser than the panel.

The preview gains the step it was missing: `Panel::show` applies the device's dark-end
collapse, so the studio stops flattering the darks by up to three codes.
`crates/art/tests/sender.rs` now compares the studio's preview with the simulator's
decoded frame *through the same model* and still passes pixel-for-pixel, indexed and
continuous. All 55 `crates/art` lib tests pass unchanged - including the clocks'
"every treatment keeps the palette exact" and `overland`'s "no colour lives in the
shadows", so the finer quantisation splits no palette and merges none.
