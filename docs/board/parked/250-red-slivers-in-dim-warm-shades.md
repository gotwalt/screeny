---
id: 250
title: Firmware / art - "bright red shaves" seen sometimes in dim warm shades: confirm it is colour breakup, then decide whether anything should change
type: research
hardware: owner's eye only (no flashing needed for the first two checks)
depends: [248, 188]
owner:
branch:
---

## Goal

On fw 0.9.0 (card 248) the owner sometimes sees "weird bright red shaves" on the clocks patch
(2026-09-21). Find out what they are. Only then decide whether to do anything.

## Context

Not measured; this is the orchestrator's reading of the code. Device numbers were clean while he
saw them (LIVE, 30.0 fps, 0 stale / superseded / decode / reject), and the dither can only ever
bump a channel by one level, so a fault is the less likely explanation.

The likely one is **colour breakup**. `display::render` uses one threshold per pixel per refresh
for all three channels, so the channels' on-times nest. A dim warm pixel such as sRGB (30, 24, 16)
has red on 13 refreshes in 16, green 9, blue 5: every cycle it is white-ish, then yellow, then
**pure red for 4 refreshes**, then off. A steady eye fuses that into the right colour. A moving eye
(a glance across the panel, or tracking a moving hand) smears single refreshes apart and the
red-only ones show as slivers - the DLP rainbow effect. It was there before 248 too, buried inside
the 9.6 Hz blink.

## The checks, cheapest first

1. Ask the owner where and when: on dim pixels and soft hand edges, during eye or hand movement,
   never brighter than a dim red = breakup. As bright as the lit hands, or in pixels that should be
   black = a real fault; stop and chase that instead (tearing between `render` and the DMA scan is
   the first suspect).
2. Clocks -> Dark ramp -> "Aligned dark" / "aligned warm" (card 188). Aligned shades do not dither,
   so breakup disappears from the held ghost dots; it can remain on moving hand edges.
3. The `frac-bits-3` build (`0.9.0-f3`, how to build it is in card 248's Log): a shorter cycle,
   fewer red-only refreshes.

## If it is breakup and he wants it gone

- Art side, free: aligned dark ramp for held shades; desaturate the darkest shades so they are not
  red-dominant (new work in the clocks patch).
- Firmware side: do **not** give each channel its own threshold - that adds green-only and
  blue-only refreshes and makes it worse. A shorter cycle is the only firmware lever.

## Acceptance

The owner knows what the slivers are, and either says they are fine or a build/art card exists
for the chosen fix.

## Log

### Parked 2026-09-21 (owner: "flag those followups for later, i think it's good enough for right now")

Written straight into parked/ so the observation and the reasoning are not lost.
