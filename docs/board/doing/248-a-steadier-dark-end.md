---
id: 248
title: Firmware - a steadier dark end: sub-level bit planes from a narrower output-enable window, and a dither that does not blink at 10 Hz
type: build
hardware: yes (the orchestrator flashes or OTAs; the owner judges by eye; no camera)
depends: []
owner: opus worker (firmware session, 2026-09-21)
branch: card/248-a-steadier-dark-end
---

## Goal

With the device's temporal dither on, the colours closest to black visibly blink at
several hertz when you are within a few feet of the panel (owner, 2026-09-21, a mostly
monochrome clock patch). With the sender snapping to the 64 bit-plane levels instead
(`output.panel: bit_planes`) the blinking stops and the intermediate dark shades go with
it. Give the panel a dark end that has the shades **and** holds still.

## What is going on (read from the code, not yet measured)

- Nothing in the LED drive itself is slower than the 154 Hz refresh. The several-hertz
  component is the dither: `FRAC_BITS = 4` (`firmware/src/gamma.rs`) means a 16-refresh
  cycle, **9.6 Hz**, and `display::render` walks the threshold through the phases in
  counting order (`t = (phase + bayer) & 15`, bump when `frac > t`). So a pixel wanting
  half a level is lit for 8 consecutive refreshes and dark for 8 - a 9.6 Hz square wave -
  and a pixel wanting 1/16 of a level is one flash per 104 ms. The Bayer offset decorrelates
  neighbours; it does nothing for one pixel, and at this pitch the eye resolves one pixel.
  Below level 1 the alternation is black <-> lit, which is the most visible case there is.
  (Card 007 measured "no periodic structure" as a *mean luminance* wobble over the panel;
  that is the spatial average the Bayer offset buys, and not what a person two feet away sees.)
- The root of it is that the smallest unit of light is too big. Level 1 is one
  output-enable window per refresh (9 slots of 64 at the default brightness, 5 at the
  Studio's usual 56) and there is nothing between that and nothing, so the dither makes
  the in-between values by *skipping whole refreshes*.
- The vendored framebuffer can do better. Output-enable is bit 8 of **every entry**, and
  each bit plane has its own copy of the rows
  (`firmware/vendor/hub75-framebuffer/src/bitplane/plain/frame.rs`, `vendor/README.md`),
  so the OE window can differ **per plane**. A plane shown once per refresh (like plane 0)
  with half the window is a real half-level at 154 Hz: a narrower pulse every refresh
  instead of a full pulse every other one. That is BCM extended below the LSB by pulse
  width instead of by repetition count, and it costs one extra plane pass in 63 (~1.6% of
  the refresh rate) and ~4 KB of DMA memory per plane - not the halving of the refresh
  rate that a 7th ordinary plane costs (card 001: 7 planes = 76 Hz, flickers).

## Steps, cheapest first - each one is a build the owner can look at

1. **Bit-reverse the dither phase.** Walk the threshold 0, 8, 4, 12, 2, 10, ... instead of
   0, 1, 2, ...: half a level then alternates every refresh (77 Hz), a quarter is 38 Hz,
   and only the 1/16-weight component is left at 9.6 Hz. Same mean light, by construction -
   keep a host test that every `q` still averages `q/16` over a full cycle, for every Bayer
   offset. Keep the per-pixel offset (check it still decorrelates neighbours after the
   reversal).
2. **Make `FRAC_BITS` a thing that can be compared by eye** at 4, 3 and 2 (a build-time
   constant is enough; a bench-only control op is fine if it is cheap). At 2 the slowest
   component is 38 Hz and the panel still resolves ~250 duty steps. The owner picks.
3. **Sub-level planes.** In the vendored fork, let `bcm_sequence` carry planes below plane
   0 that are shown once per refresh with a narrowed OE window: a half plane, and a quarter
   plane if the window allows. The window is a whole number of slots and depends on the
   runtime brightness, so: define exactly what each sub-plane's width is at each of the 25
   brightness steps, what happens when it rounds to zero (that plane is simply off and its
   weight goes back to the dither), and keep `set_oe_slots` the one place that writes OE.
   The gamma table already carries four fractional bits, so `render` has the information;
   the top fractional bits go to the sub-planes and the rest stay with the (shortened,
   bit-reversed) dither. Brightness must still cost no depth at the top, the cap
   (`MAX_OE_SLOTS`) must not move, and card 136's brightness floor must be respected.
4. **The undithered path rounds instead of truncating.** `q >> FRAC_BITS` sends sRGB 23-33
   to black and puts 22 of the brief's 64 "level" codes on the level below (e.g. 125 -> 12,
   149 -> 18, 156 -> 20). Round to nearest. Separately, consider a dead zone in the dithered
   path: a remainder of 1/16 or 15/16 is a one-refresh blip every 104 ms that carries no
   visible light - snap it to the level.

Stop after any step if the owner says the picture is right; record which steps shipped.

## What is not known until it is on the panel

- **How narrow a pulse the panel's driver chips pass linearly.** One slot is one pixel
  clock; a 2-slot window is a few hundred nanoseconds. The half plane may come out dimmer
  than half, or the quarter plane as nothing. That is what the ramp below is for; if a
  sub-plane is not roughly monotonic, drop it rather than correct for it.
- RAM: ~4 KB of DMA-capable memory per added plane, against research 009/010. Say what the
  headroom is before and after.
- Render cost: `render` is ~3.1 ms of a 6.5 ms refresh with dither on (card 030). More
  planes written per refresh must not push the display task past a refresh.

## Deliverables

- `firmware/src/display.rs`, `firmware/src/gamma.rs`, and the vendored
  `hub75-framebuffer` changes, with `firmware/vendor/README.md` updated to describe them.
- A bench pattern for the owner's eye: a held dark ramp (sRGB 0-70 in a warm monochrome
  and in neutral grey, steps wide enough to tell apart), sendable with the existing sender
  (`crates/screeny` patterns). No camera.
- **The host model follows the device.** `crates/panel` (`DEVICE`, `DITHER_PHASES`, the
  steps count) is checked entry by entry against the firmware; whatever ships here changes
  it, and `docs/design/generative-art-brief.md` section 2.1 and 2.1.1 with it. Card 188
  (the sender's level alignment) reads the same model - leave it a note of the new level
  structure.
- `screeny stats` before and after on a 30 fps stream: refresh rate, fps, drops.

## Acceptance

- The owner, within a few feet, sees no blinking on the dark ramp and on the clock patch
  with the device dither on, and can tell more dark shades apart than with
  `output.panel: bit_planes` today.
- Host tests: mean light per `q` unchanged (or changed only by the step 4 rounding, stated);
  the sub-plane widths are a pure function of brightness with a test at the floor, the
  default, 56 and the cap.
- Refresh rate stays at or above ~145 Hz; the stream holds 30 fps with 0 drops for a
  10-minute run on the final build (one run, per bench discipline).
- No change to the wire protocol. If one turns out to be needed, stop and say so.

## Log
