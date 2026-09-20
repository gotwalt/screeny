---
id: 066
title: The simulator dims the panel the way the device used to, not the way it does
type: build
hardware: no
depends: [016, 020]
owner: worker-066
branch: card/066-sim-brightness-model
---

## Goal

Make the simulator's brightness model the one card 020 shipped, so that
`screeny brightness 80` looks the same on the panel and in the LED-dot window.

## Context - found while doing card 016

`crates/panel`'s `Lut` (which was `crates/sim/src/panel.rs`, and before that
card 006's) applies brightness as a **linear scale before quantising**:

```rust
lin_to_srgb8(panel.quant_lin(SRGB_TO_LIN[v] * scale))
```

At brightness 128 that leaves about half the duty steps, so the simulator's
picture at half brightness is visibly more banded than at full. The source
comment says so: "scaling before quantising is what card 020 exists to fix, so
the simulator models the unfixed version."

Card 020 is **done**. The device dims by shortening the output-enable window
(`display::slots_for`), which costs light and not bit depth - every code value
keeps its own duty step, the panel is just lit for less of each row. So the
two now disagree in the one direction that matters: the simulator predicts
banding the device does not have, and the bench brightness cap of 160 is
exactly the range where this shows.

## Deliverables

- `screeny_panel::Lut` (or a second constructor beside it) models
  output-enable dimming: quantise at full depth, then scale the *emitted*
  light, so `distinct_levels()` does not fall with brightness.
- Keep the old one if a "dim by scaling" model is still wanted for anything,
  but the simulator's default must be the device's behaviour.
- A test that the number of distinct output levels is the same at brightness
  255 and at 128, and `crates/sim`'s existing brightness tests still pass.
- If the numbers move, say what a camera would see: card 061 (fixed-exposure
  capture) is parked, so this may be judged in the window rather than on the
  bench.

## Acceptance

At brightness 128 the simulator shows the same 64 duty steps as at 255, dimmer;
`crates/sim`'s panel tests pass; the comment claiming to model the unfixed
version is gone.

## Log

### The device's model, read out of the firmware (2026-09-19)

Read `firmware/src/display.rs` and `firmware/src/gamma.rs` rather than trusting
card 020's prose. They agree, and they are more specific than the card:

- `gamma.rs` is the sRGB EOTF and nothing else. Its own comment: "Brightness is
  deliberately **not** in this table. It is output-enable duty in the
  framebuffer, so dimming the panel does not move a single entry here and costs
  no colour depth." All 64 duty levels exist at every brightness.
- `display::slots_for(b) = (b * 25 + 127) / 255`, lit output-enable slots out of
  `MAX_OE_SLOTS = 25` (39% duty, Tidbyt's documented maximum, the USB budget).
  Linear in duty and therefore linear in emitted light.
- **The control has 25 steps, not 256.** Brightness 129 and 130 are the same
  number of slots and therefore the same picture; 1..=5 are *zero* slots, i.e.
  fully black. The simulator now reproduces both, because a preview that
  smoothly interpolates where the device steps is still lying, just politely.

### `screeny_panel::Lut` now dims the window, not the values (2026-09-19)

`crates/panel/src/model.rs`:

- Added `MAX_OE_SLOTS`, `oe_slots(b)` (a copy of `slots_for`, with a test that
  pins it to the firmware's numbers) and `oe_light(b) = slots/25`.
- `Lut::new` now quantises at full depth and scales the emitted light:
  `lin_to_srgb8(panel.quant_lin(SRGB_TO_LIN[v]) * oe_light(b))`.
- The old order is kept as `Lut::value_scaled`, and it is used by one test and
  nothing else. It stays because it is the thing `DIMMED` names, and because
  the difference between the two models is the evidence for this card; if a
  future receiver ever dims by scaling values, it is already written down.
- `Lut::distinct_levels()` added, `(distinct output codes, codes crushed to
  black)`, the brightness-aware sibling of `Panel::distinct_levels()`.
- Doc comments: the "so the simulator models the unfixed version" paragraph is
  gone; `DIMMED` now says it is the pre-020 loss and not "today"; `DEEP` no
  longer says the driver might one day get OE brightness (it has it).

Measured, `NOMINAL` (64 levels), `(distinct output codes, codes crushed to 0)`:

| brightness | OE slots | light | new model | old model | white maps to |
|---|---|---|---|---|---|
| 255 | 25 | 1.00 | **(64, 22)** | (64, 22) | 255 |
| 160 (bench cap) | 16 | 0.64 | **(64, 22)** | (41, 30) | 209 |
| 128 | 13 | 0.52 | **(64, 22)** | (33, 34) | 191 |
| 96 (device default) | 9 | 0.36 | **(64, 22)** | (25, 40) | 162 |
| 64 | 6 | 0.24 | **(64, 22)** | (17, 50) | 134 |
| 40 | 4 | 0.16 | (62, 22) | (11, 64) | 111 |

So at the bench cap the old model threw away 23 of the 64 steps and crushed
eight more sRGB codes to black than the panel does. That is the banding the
window showed and the panel never had.

The 22 crushed codes are the panel's own (`Panel::new(6).distinct_levels()`);
they do not grow as it dims, which is the other half of "costs light, not
depth". At brightness 40 the count drops to 62 - not the panel banding but the
*preview* running out of room: two pairs of adjacent duty steps round onto one
8-bit screen colour once the whole ramp is squeezed into 0..111. Documented on
`Lut::distinct_levels`. A camera would see 64 steps there; a screenshot of the
window would not.

`crates/sim/src/panel.rs`: module doc rewritten to say brightness is the
output-enable window, and two tests - the existing one gains "brightness 0 is
black", plus `dimming_the_window_keeps_every_duty_step` asserting the level
count is equal at 255, 160 and 128 while the white point falls.

Nothing else in the workspace builds a `Lut`: `crates/screeny/src/panel.rs`
re-exports the type but never constructs one, and `crates/demos` only uses
`Panel`. `lab/` has its own private copy of the model and is untouched. So the
only pictures that move are the simulator's window, its `--dump-dir` PNGs at
non-255 brightness, and the idle `DIM` screen (`screens.rs`, brightness/10).
