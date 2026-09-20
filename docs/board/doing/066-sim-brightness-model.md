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
