---
id: 188
title: Art - held dark shades sit on a panel level: the aligned-code table in the panel model, a dark-end snap that leaves the brights alone, and the clocks patch on it
type: build
hardware: no (the owner looks at the result on the panel through the Studio)
depends: []
owner: worker (sonnet)
branch: card/188-held-dark-shades
---

## Goal

The owner's clocks patch (2026-09-21) has two bad choices today. With
`output.panel: dithered` its dim shades - the ghost dots, the soft edges of the hands -
blink at several hertz up close, because they sit between hardware levels and the device
makes them by alternating "off" and "on" at 9.6 Hz. With `output.panel: bit_planes` they
hold still, and some of them disappear, because the whole frame is flattened to 64 levels.
Make the third choice: **keep the device's resolution where it is invisible, and put what
is held and dark exactly on a level.** `docs/design/generative-art-brief.md` section 2.1.1
is the guidance this card implements; read it first.

## Context

- A code is steady when its duty in the firmware's gamma table (`firmware/src/gamma.rs`,
  `SRGB_TO_Q`, sixteenths of a level) is a whole level. For levels 0-16 the lowest code
  that reaches the level is within 2/16 of it and is right whether the device dithers or
  not: `0 34 50 62 71 80 87 94 100 106 111 116 121 126 130 134 138`.
- `screeny_panel::NOMINAL.snap` / `screeny_art::panel::Panel::BitPlanes.snap8` answer a
  different question - the *nearest* code to a level's light - and 22 of those 64 codes sit
  a hair under their level (125, 149, 156, ...). Harmless with the device dither on; one
  level low with it off. One implementation: the table belongs in `crates/panel`, derived
  from the same data the existing firmware-table check uses, not typed in.
- Card 248 (firmware) will add steadier sub-levels below level 1 and change the dither.
  This card must read the level structure from the model so that 248 changes one place.
  It does not depend on 248 and should not wait for it.

## Deliverables

1. `crates/panel`: the aligned code for each level (lowest code reaching it), how far a
   given code is from its nearest level in sixteenths, and a test against the firmware
   table beside the existing one.
2. `crates/art` (`panel.rs` and wherever the hand-over quantises): a dark-end snap -
   a channel whose target is below a threshold level (start at 16, make it a named
   constant) goes to the nearest aligned level as the **last** step, after blending and
   anti-aliasing; channels above it are untouched and keep the 1008-step path. Decide,
   and say in the Log, whether this is a third `output.panel` value or a separate
   `output` flag; the default must not change any existing patch's pixels (golden tests).
3. The clocks patch (`crates/art/src/patches/clocks`): its dark ramp designed as a short
   list of level triples per 2.1.1 (per-channel alignment means the darkest shades lose
   hue control - choose them, do not compute them), using the new snap. Offer the owner
   two or three variants to look at in the Studio rather than one.
4. Optional, only if it falls out cheaply: the "buy levels with brightness" trade from
   2.1.1 as a worked number for clocks (its peak level today, the scale factor, the
   brightness that compensates, against card 136's floor and card 187's policy). A note
   in the Log is enough; do not build a mechanism.
5. `docs/design/generative-art-brief.md` 2.1.1 updated to name the real API.

## Acceptance

- `cargo test` and `cargo clippy --workspace --all-targets` clean; goldens for every
  other patch byte-identical.
- A host test that a clocks frame, after the snap, has no channel value below the
  threshold that is off-level by more than 2/16.
- The owner, within a few feet of the panel with the device dither on, sees the clocks'
  dim shades present and not blinking.

## Log

### 2026-09-21 - shaping note from the firmware session (card 248), folded into Context

Not new scope - a constraint on how deliverables 1-2 are built, so future firmware work
(card 248) has exactly one place to change:

1. The snap must take "the list of steady duty values" as **data** read from `crates/panel`
   (derived from the same table the existing firmware-table check uses), rather than
   `crates/art` assuming duties land every 16 sixteenths. 248 will probably add one or two
   steady sub-levels below level 1 (a half level near sRGB 21-22, maybe quarters near 12 and
   28), and that set may become a function of runtime brightness. This card does not build
   for that - it only avoids baking `* 16` arithmetic into `crates/art`, and instead calls
   into a `crates/panel` function (built from [`DITHER_PHASES`], not a literal `16`) that 248
   is free to change.
2. 248 will bit-reverse and likely shorten the dither cycle, so the threshold level (16) may
   come down later - kept a named constant per the card's own instruction.
3. `crates/panel/src/model.rs`'s existing constants (`DEVICE`, `DITHER_PHASES`, the steps
   count) are not touched by this card, only added beside - 248 changes those at its own end
   and rebases onto this merge.
4. Fact to have right: `output.panel: bit_planes` is sender-side only choice of which codes
   to send; the device's temporal dither stays on in both Studio modes. `bit_planes` looks
   steady because nearest-level codes happen to have tiny remainders, not because the device
   stopped dithering.

### 2026-09-21 - worktree was behind main

The worktree this card started in was at commit 127c829 (card 178, several commits before
the board was pruned to just 188/187/199). Re-branched from `main` at 6bdcc1c per the
coordinator's instruction; `card/188-held-dark-shades` now starts there.

### 2026-09-21 - deliverable 1: the aligned-code table in `crates/panel`

Added to `crates/panel/src/model.rs`, beside the existing `DEVICE`/`DITHER_PHASES`
constants and their firmware-fixture test:

- `duty_16ths(v: u8) -> u32` - an sRGB8 code's duty in sixteenths of a level, recovered
  from `DEVICE.emit1(v) * DEVICE.max()` rather than typed in from the brief or the
  firmware. New test `duty_16ths_is_the_firmwares_srgb_to_q` checks it against the
  `FIRMWARE_SRGB_TO_Q` fixture **to the integer**, not `DEVICE.emit1`'s 1e-6: an aligned
  code is chosen by comparing duties directly, and a float wobble at a boundary would
  pick the wrong one.
- `nearest_level(duty_16ths) -> (level, signed offset)` - any duty's nearest whole level
  and how far off it is, round-half-up (matches the firmware's own `SRGB_TO_Q` rounding).
- `AlignedLevel { level, code, offset_16ths }` and `aligned_levels(max_level) -> Vec<_>` -
  the lowest sRGB8 code that *reaches* each level (`duty(code) >= level * DITHER_PHASES`,
  the brief's "lowest code that lands on"), and how far past the level it actually sits.
  Reverse-engineered the exact rule from the brief's own 17-value list: it is **not**
  "nearest by rounding" (that would put sRGB 21, duty 8, at level 1 - it is not, per the
  existing `dark_srgb_is_no_longer_off`/`BitPlanes` tests) - it is "smallest code whose
  duty has reached or passed the level's floor". Checked every one of the 17 listed
  values (0, 34, 50, ... 138) by hand against `FIRMWARE_SRGB_TO_Q` before writing the
  function, including the worst case (level 13, sRGB 126, 2/16 short) - the new test
  `aligned_levels_match_the_brief` pins all 17.
- `nearest_level_agrees_with_the_aligned_table`: every aligned code's own nearest level is
  the level it was chosen for (never the one below) - the two functions can't disagree
  about an aligned code.

No literal `16` in any of it - `DITHER_PHASES` throughout - per the firmware session's
shaping note above.

`cargo test -p screeny-panel --release`: 17/17 green (was 15). `cargo clippy -p
screeny-panel --all-targets`: silent.

Deliverable 1 is `screeny_panel::{duty_16ths, nearest_level, AlignedLevel,
aligned_levels}`, re-exported from the crate root.

### 2026-09-21 - deliverable 2: `Panel::AlignedDark`, the third `output.panel` choice

**Third `output.panel` value, not a separate flag.** The Goal section itself frames this
as "the third choice" among panel behaviours (`dithered` blinks dark and held, `bit_planes`
throws away the bright gradients the dither handles well) - a third enum value says that
directly, and picking it is exactly the same *kind* of decision as the existing two,
switched the same way. It also does the least damage to the "default must not change any
existing patch's pixels" rule: `Panel::default()` stays `Dithered`, so nothing that does not
explicitly ask for `AlignedDark` is touched. A boolean flag layered on top of `Dithered`
would have needed its own default-off story to get the same guarantee, for no benefit -
`output.panel` is already the setting that answers "which codes get chosen", and this is a
third answer to that one question, not an orthogonal one.

**`crates/art/src/panel.rs`:**

- `Panel::AlignedDark` - third variant, `model()` maps it to `screeny_panel::model::DEVICE`
  (the same physical panel as `Dithered`: only code *selection* differs, so `emit`, `shown`,
  `distinct_levels`, `steps` all agree with `Dithered` exactly - pinned by
  `aligned_dark_is_the_same_device_as_dithered`).
- `DARK_ALIGN_LEVEL: u32 = 16` - the threshold, a named constant per the card (card 248 may
  bring it down with a shorter dither cycle).
- `level_code(level) -> u8` - the code for a level, from `screeny_panel::aligned_levels`,
  cached once (`OnceLock`, matching `crates/art/src/color.rs`'s own `linear_to_srgb`
  pattern) rather than per-pixel. One table, read by both the snap and `level_triple`.
- `level_triple([u32;3]) -> Rgb` - three levels in, linear light out via `level_code` per
  channel. This is "the new snap" deliverable 3 uses to turn a hand-picked level triple into
  an actual colour.
- `Panel::code` special-cases `AlignedDark`: below the threshold it ignores the dither bias
  entirely (the whole point - the same target always gets the same code, held or not) and
  returns `level_code` of the target's nearest level (plain rounding against `NOMINAL`, i.e.
  the *un-dithered* judgement of which level a colour is nearest); at or above the threshold
  it falls through to the ordinary `Dithered` path, bias included.

Tests added: `aligned_dark_is_steady_below_the_threshold_and_dithered_above_it` (bias moves
nothing below the line, matches `Dithered` exactly above it),
`every_aligned_dark_code_is_close_to_its_level` (every code `level_code` can return is
within 2/16 of the level it was chosen for - re-proving crates/panel's own claim from this
crate's side), `level_triple_is_three_aligned_codes`, `aligned_dark_is_the_same_device_as_dithered`.

**CLI**: `crates/art/src/bin/screeny-art.rs`'s `--panel` gained `aligned-dark` /
`aligneddark` beside `dithered`/`bit-planes`, for `screeny-art snapshot --panel aligned-dark`.

**What the Studio page needs, not built (out of scope: `crates/studio` is cards 187/199's).**
`crates/studio/ui/index.html`'s `#panel-kind` fieldset is hand-authored HTML - two
`<label><input type="radio" name="panel" value="...">` rows (`dithered`, `bit_planes`) - and
`picture.js`'s `bindRadios` (in `common.js`) is fully generic over whatever radios that
fieldset holds, so the *only* change needed is one more label:

```html
<label title="Dithered above level 16, aligned onto an exact level below it">
  <input type="radio" name="panel" value="aligned_dark"><span>Aligned dark</span></label>
```

added inside `<fieldset class="seg" id="panel-kind">` (`crates/studio/ui/index.html`,
currently lines 152-158), value `aligned_dark` (the enum's own `serde(rename_all =
"snake_case")` spelling). Still three stops, so `.seg` (card 163's rule: three or fewer is a
segmented control) does not need to become a `<select>`. No JS change - `bindRadios` reads
whatever radios are there. This was checked, not guessed: read `picture.js:549` and
`bindRadios` in `common.js` before writing this note.

`cargo test -p screeny-art --release`: 10/10 in `panel::`, up from 6. `cargo clippy -p
screeny-art --all-targets`: two `clippy::doc_lazy_continuation` warnings from a `-` that
read as a markdown list start mid-paragraph; reworded, no code change, now silent.

### 2026-09-21 - deliverable 3: the clocks patch's dark ramp

`crates/art/src/patches/clocks/draw.rs`: `RestScale { ink, reach, shade: Option<Rgb> }`
replaces the old `[ink, reach]` tuple `Dials::rest` carried. `shade`, when `Some`, overrides
`tint.scale(ink)` outright for that dial's hands and is added to the built palette verbatim
(`if !colours.contains(&shade) { colours.push(shade) }`) - it is its own exact entry, not
something left to `Palette::map`'s nearest-match to land on by luck. `rest: &[]` (every other
caller, and `clocks-dials`) is unaffected: `.get(i).copied().unwrap_or(RestScale::FULL)` is
the same "not present -> full strength" behaviour the old code had.

`crates/art/src/patches/clocks/mod.rs`:

- `DARK_RAMPS: &[&[[u32;3]]]` - two hand-picked variants. `"neutral"`:
  `[[1,1,1],[2,2,2],[3,3,3]]`, equal levels, the grey the ramp already fades towards.
  `"amber"`: `[[1,1,0],[2,1,1],[3,2,1]]`, leaning warm per the brief's own example
  ("(2,1,1) and (2,2,1)"). Chosen by reading the numbers, not by rendering and looking - the
  owner judges the panel, not this worker; both variants and the "before" picture are in the
  snapshot set below for that judgement.
- `choice("dark", "Dark ramp", DARK_CHOICES, DEFAULT_DARK as f32)` - a new patch parameter,
  declared the same way card 163's other named choices are (`rest`, `dance`). No Studio
  change needed: `crates/studio/ui/picture.js` already renders any `ParamSpec` with
  `choices` generically (segmented control at <=3 stops), unlike `output.panel`'s
  hand-authored radios.
- `dark_shade(variant, ink) -> Rgb` picks a rung of the chosen ramp and converts it with
  `screeny_art::panel::level_triple` - deliverable 2's "the new snap" the card asks
  deliverable 3 to use.
- `picture()`: `shade = (settled >= 1.0 && rest.ink < 1.0).then(|| dark_shade(...))`. Only
  once truly landed, never mid-fade - the 0.6 s settle is moving content (brief: "leave
  everything else to the dither"), so it keeps the old continuous `tint.scale` the whole way
  and only the final, held colour is aligned. `RESTS[0]` ("as it was", `ink == 1.0`) never
  gets a shade at all - it is kept undimmed on purpose, for comparison, exactly as its
  existing test comment already said.

**Tests.** `every_treatment_keeps_the_palette_exact` now expects 32 colours, not 31, once a
treatment that actually dims (`ink < 1.0`) has landed (`settled >= 1.0`) on a time that
actually leaves a dial resting - found by running it first and reading the failure: two of
the seven `AWKWARD` times (`9:05`, `0:00`) draw digits with no rest cells at all (`0`, `5`,
`9` have none), so `want` has to check `resting(hh, mm)` too, not just the treatment. Still
`<= GUARANTEED_PALETTE` (32 == 32), still exact.

New test `held_dark_shades_land_within_two_sixteenths_of_a_level` (the card's acceptance
line) renders a clocks frame at 21:12 for every `RESTS` treatment, runs it through the real
`Pipeline` with `Output { panel: Panel::AlignedDark, .. }`, and checks every wire byte: if
its nearest level is below `DARK_ALIGN_LEVEL`, the offset is within 2/16. First write used
`Pipeline::default()` (plain `Dithered`) and failed on `"as it was"` at level 15, offset -7 -
correctly, because that treatment is undimmed on purpose and its hand edges are ordinary
anti-aliasing, never snapped by deliverable 3's patch-side fix. Re-read the acceptance line
("a clocks frame, after **the snap**") and switched to `Panel::AlignedDark`: deliverable 3's
hand-picked ramp is the better-looking fix for the common case (resting ink, the default
`Dithered` path), deliverable 2's pipeline-level snap is the backstop that catches everything
else (hand edges, a treatment that is deliberately left undimmed) - which is exactly the
division of labour the two deliverables were always going to need.

`cargo test -p screeny-art --release`: 123/123 (was 117 before this card; `clocks::` alone is
19/19, up from 15). Every other patch's tests (`vesta`, `flock`, `overland`, `metaballs`,
`knot`, `lattice`) untouched and still green - `clocks-dials` passes `rest: &[]` and is
provably unaffected. `cargo clippy -p screeny-art --all-targets`: clean.
