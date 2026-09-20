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
   distinct greys to 43** (step 5 measures it). That is the card's acceptance in one
   number: the ramp was four bands and a third of it black, and it is now a ramp.
2. The dither bias is one **duty step**, not one of 64 levels. Above sRGB 38 a duty
   step is a tenth of an 8-bit code, so ordered dither now rounds away to nothing
   in the midtones - at most one code, and only on a code boundary; no texture the
   panel cannot show, and nothing spent on the wire compressing noise. Below 38, where up to four codes share a level, a duty step is
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

### Step 3 - the colour budget: 32 is a guarantee, not a ceiling

`frame::MAX_PALETTE` was 32, called "largest palette that is still an exact frame".
Since card 101 the encoder answers for itself, so the constant is split:

- `GUARANTEED_PALETTE = 32` - exact **whatever the index plane looks like**, because
  the fixed-rate `PAL5` rung is 1376 bytes for any 32 colours and any 2048 indices,
  pure noise included.
- `MAX_PALETTE = 256` - the ceiling. Between the two, exactness is a compression
  question about the picture, and `Measured::exact` is the measured answer.

`Palette::always_exact()` is that distinction with a name. New test
`a_large_palette_still_goes_out_exactly`: four 16-step OKLCH ramps (65 colours), a
terraced gradient mapped onto them, through the real encoder - exact, `pal8-lz`,
inside the 1464-byte budget.

One constant was doing two jobs: the GPU shader's uniform `palette` array is now
`gpu::fragment::SCENE_PALETTE` (32, matching `fragment.wgsl`), so raising the frame's
ceiling does not silently make every GPU piece's uniform 4 KB. **No pixel changes** -
every piece still builds 31 or 32 colours, and `plasma`'s "Colours" slider still caps
at 32 (its `ParamSpec` always did).

### Step 4 - the studio: the control, the meter, and the stored setting

- **Panel control.** "Levels per channel 64 / 32 / 16" becomes "Panel: Dithered /
  Bit planes", with a hint line saying what the second is for. The two are
  `screeny_panel::DEVICE` and `NOMINAL`.
- **Stored state.** `settings.levels` leaves the file. No schema bump: the shape did
  not change, serde ignores the key, and the panel comes up on the device's model.
  `state::note_retired_levels` reports it once in `repaired` - the same list as every
  other silent correction - because a setting that quietly stops meaning what the
  person chose is worse than one sentence on the dashboard. Test
  `a_retired_levels_setting_loads_and_is_reported` loads a v3 file holding 32, 16 and
  64 in turn: the rest of the file survives (seed, dither, `panel_model`), the panel is
  `dithered`, it is said once and not per player, and it is a repair rather than a
  recovery. The v1 and v2 migration tests now expect that one sentence, because every
  file of those vintages names `levels`.
- **Colours meter.** Was 0..64 with "of 32 exact" and ticks at 16 and 32. Now 0..256
  with the ticks kept at 16 and 32 (6.25% and 12.5%), and a note that says which of
  three situations the frame is in: guaranteed exact at or under 32, exact because the
  index image compressed, or requantised. All three come from `Measured`, not a rule.
- **Frame size meter.** Already measured rather than estimated (card 101): it reads
  `Measured::bytes` against `meter::PAYLOAD_BYTES`, and names the codec the chooser
  really picked. The only change is that it now says "lossy" when the frame is.

### Frame rate: what happens today, unchanged

Asked to write it down rather than change it. As it stands:

- The **player renders at `fps`** (1..60, default 60, card 172) and hands every frame
  to `Link::send`. The link never sleeps.
- `Cadence::Limit` is the default: the link drops frames that arrive before the next
  slot on an absolute schedule at the rate it is currently targeting, which starts at
  `SenderConfig::fps` and follows spec 6.9's ladder down under loss and back up. **A 60
  fps piece into a 30 fps panel therefore puts 30 on the wire and the device supersedes
  nothing.** Half the frames come back `Sent::Coalesced`, which is the system working,
  not a fault. `Cadence::Free` sends everything and lets the device count the surplus
  as `frames_dropped_superseded`; nothing in the studio selects it.
- The four counters exist in `PanelStatus` and three are on the page, in the Panel
  block: `Frames  N sent, M folded, K lost` (`frames_sent`, `frames_coalesced`,
  `frames_dropped`), plus `Link  up - N fps` (`PanelStatus::fps`, the rate the panel is
  keeping up with) and `Rendered  N frames at M fps` (the player's own measured rate).
- **What is missing:** `frames_offered` is carried but not displayed - it is
  recoverable as sent + folded + lost, so this is cosmetic. Nothing shows the *ladder*
  moving: `PanelStatus::fps` is the current target, but a step down under loss and the
  step back up are not distinguishable from a piece being set to a lower rate, and
  there is no history. That is the one thing a person watching a bad WiFi day would
  want and cannot see. Written up as card 113 rather than done here.

### Open with the owner

Three things that are now **style rather than necessity**. The dark end is usable -
only sRGB 0 and 1 are black - so each of these is a choice the panel no longer forces.
Left exactly as they are; none of them changed a pixel in this card.

1. **`overland` cuts palette colours below OKLCH L 0.3 to true black**
   (`pieces/overland.rs`, `fn paint`). Its comment says "below this the panel has a
   handful of levels and they carry colour casts". The first half is no longer true;
   the second half still is. *What would change if it went:* at dusk the sky would fade
   through dim bands instead of going out one band at a time, zenith first, and night
   terrain would be very dark colour rather than silhouette. The test
   `no_colour_lives_in_the_shadows` encodes the current rule for every hour, so this is
   a deliberate look, not an oversight. It may well be the better look.
2. **The clock pieces' hand ramps start at OKLCH L 0.32** (`pieces/clocks/draw.rs`,
   `const DARK`, with `STEPS = 15`). Comment: "anything dimmer is left black, because
   the panel has almost no levels down there". *What would change if `DARK` dropped:*
   anti-aliased hand edges would get dimmer steps, so strokes would look smoother and
   thinner at the tips; the resting-dial treatment (a fifth of the ink) would land
   further down its own ramp and could go quieter still. The palette must stay at 31
   colours to keep the frame exact, so a lower `DARK` means a longer, darker ramp over
   the same 15 steps, not more steps.
3. **`Palette::ramps`'s `dark` argument.** Its advice - "keep it around 0.4 or above" -
   was corrected in the doc comment, but no caller's value was touched. `knot` and
   `overland` choose their own.

A fourth, smaller one: the **panel-model A/B** now offers "Dithered" and "Bit planes".
If the owner would rather see a dim room modelled, that is a brightness control on the
preview (`screeny_panel::oe_light`), not a panel - say so and it is a small card.

### Step 5 - the test card's dark ramp, which is this card's acceptance

The acceptance line asked for a camera capture beside the studio's preview. The
camera is disconnected (owner, 2026-09-19), so the evidence is the test card's own
dark ramp, measured. Row 4 is the darkest quarter of sRGB stretched across all 64
columns - one column per sRGB code, near enough - and
`pieces/testcard.rs`'s new tests read it straight out of `Output::preview`, with the
codec preview off (a test card is deliberately the worst case for the encoder, and
this row is a question about the panel).

```
device   0  0  3  3  3  7  7  7  7 10 10 10 13 13 13 15 15 18 18 20 20 22 ... 63 64
planes   0  0  0  0  0  0  0  0  0  0  0  0  0  0  0  0  0  0  0  0  0  0 ... 62 62
```

- **On the device: 43 distinct greys out of 64 columns, rising the whole way, lit
  from the third column.** That is a ramp. Its steps are uneven - pairs and triples
  at the bottom where up to four sRGB codes share a duty level - but it never goes
  backwards and it never sits on black.
- **On the bit planes alone: four values** (0, 34, 50, 62), with the first 21 columns
  black. That is what the preview drew before this card, and what the panel really
  did before card 030.

Second test, the dither change in one number: on this dark row a full-amplitude
blue-noise dither swings a pixel by at least 3 sRGB codes; on the bright half of the
grey ramp (sRGB 129..255) by **at most 1**, and then only where a value sits on a code
boundary. Before, it was a screen-door texture at every brightness, none of which the
panel could show above sRGB 38 - and all of which the LZ coder had to pay for.

What the owner should look for on the real panel: row 4 (the fifth strip from the
top) should read as a continuous dark ramp from black at the left, not as three or
four steps with a long black run. Large dark areas may sparkle faintly; that is the
firmware's phase cycle and is expected (brief 2.1).

### New card

- **113** `docs/board/backlog/113-studio-cadence-history.md` - show the sender's
  cadence ladder moving. Written out of the frame-rate note above: the rate is on the
  page but its *history* is not, so a step down under loss looks exactly like somebody
  moving the frame-rate slider. Nothing about the sender was changed by this card.
- **114** `docs/board/backlog/114-sim-window-temporal-dither.md` - `crates/sim`'s
  *window* still draws the panel at 64 levels (`Panel::levels(64)` =
  `screeny_panel::NOMINAL`), so it is now pessimistic about the darks by about 30
  sRGB codes while the studio's preview is right. Two pictures of one panel that
  disagree is what card 016 set out to stop. Not touched here: it is another crate
  and it changes that window's pixels. `Snapshot::decoded` is bit-exact and
  unaffected, which is why `crates/art/tests/sender.rs` is still a valid check.

### Green, and handed over

```
cargo test --release --no-fail-fast     79 suites, 729 passed, 0 failed  (exit 0)
cargo clippy --workspace --all-targets  silent
```

`crates/panel` 17 tests (4 new), `crates/art` lib 58 (was 51: 5 new panel tests, 2 new
test-card tests, 1 new palette test, minus the one that asserted sRGB 21 was black -
kept and inverted), `crates/art/tests/sender.rs` 3 (both preview comparisons now go
through the panel model), `crates/studio` lib 53 (1 new).

Files touched outside `crates/art` and the studio's settings/controls: `crates/panel`
(`model.rs`, `lib.rs` - the model itself, which is the point of the card) and one line
of `crates/screeny/src/panel.rs`, which re-exports `screeny_panel::model`'s public
names and would otherwise not carry `DEVICE`. Nothing in `crates/studio/src/ws.rs`,
`lib.rs`, `api.rs`, `devices.rs`, `fleet.rs` or `crates/screeny/src/discover.rs` (cards
196 and 141). Nothing in `firmware/`, `crates/proto`, `crates/receiver` or
`docs/design/protocol-v1.md`. No hardware, no serial port, no camera, no LAN: the only
thing started was `screeny-sim` on loopback with ephemeral ports, inside
`tests/sender.rs`, which bounds all its own waits. No processes left running.

### Orchestrator, after the merge (2026-09-20)

Reviewed before merging. Checked for myself that the `FIRMWARE_SRGB_TO_Q` fixture in
`crates/panel/src/model.rs` is `firmware/src/gamma.rs`'s table entry for entry (script, all
256 equal), and that `display.rs` does spend the remainder over `FRAC = 16` phases. Because
this changes the bytes the panel is sent, measured the encoder's answer on main and on the
branch with `screeny-art play ... --seconds 6` into a loopback simulator, seed 4242:
metaballs `pal8-lz` lossy ~1210 B on both; plasma `pal8-lz` exact ~1400 B, 180 of 180
exact on both; 30 fps, 0 dropped on both. So the finer grid costs the link nothing.
Merged `--no-ff`; root `cargo test --release --no-fail-fast`: 732 passed, 0 failed; clippy
silent. Not judged on the real panel by anyone yet: that is the owner's eye on the test
card's fifth strip after the next deploy, with the four "Open with the owner" items above.
