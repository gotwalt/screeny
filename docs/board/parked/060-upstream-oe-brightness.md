---
id: 060
title: Offer output-enable brightness upstream to hub75-framebuffer
type: build
hardware: no
depends: [007]
owner:
branch:
---

## Goal

Get `firmware/vendor/hub75-framebuffer`'s brightness patch into
`liebman/hub75-framebuffer` so we can drop the vendored copy, and so that
everyone else running a HUB75 panel off USB power stops being told the same
wrong thing card 001 was told.

## Context

Card 007 vendored `hub75-framebuffer` 0.12.0 to add output-enable duty control.
`firmware/vendor/README.md` has the whole story; the short version is that the
OE line is bit 8 of every framebuffer entry, written once by
`make_data_template()` and never touched again, so the lit window can be
rewritten at run time without disturbing colour data, the row address or the
latch. It costs no colour depth and no refresh rate. Card 007 measured it
working on real hardware across a 12.5x brightness range with all 16 steps of a
grey wedge intact.

The upstream README and `esp-hub75`'s documentation both say there is no
brightness control, which is what sent card 001 down the value-scaling path
that throws away three of six bits.

## What to offer

The patch as it stands is ~40 lines on `bitplane::plain::DmaFrameBuffer`:

- `const OE_SLOTS`, `const OE_DEFAULT_START`
- `fn set_oe_slots(&mut self, lit: usize)`
- `fn set_oe_window(&mut self, start: usize, lit: usize)`

Upstreaming it properly is more than that, and the extra work is the reason
card 007 did not do it:

1. The same treatment for the other layouts — `bitplane::plain::row`,
   `bitplane::latched`, and the row-major `plain`/`latched` framebuffers. Each
   has its own template function; the OE bit is in the same place but the row
   structure differs.
2. A convenience on the driver side — an `esp-hub75` `Hub75Config::brightness`
   or a `set_brightness()` that writes both buffers — since "call it on both
   buffers, it lives in the buffer not the peripheral" is a sharp edge.
3. Tests. Upstream has hardware-in-the-loop tests and good unit coverage;
   a patch without matching tests will not land.
4. A note in the README about what the duty actually is: with `trail-blank-8`
   on a 64-column panel the widest window is 55 of 64 slots, so "full
   brightness" is 86% duty, not 100%.

Worth writing the issue first and asking whether the maintainer wants it
shaped this way before building all five layouts.

## Deliverables

- An upstream issue describing the mechanism and the measurement.
- A PR if the maintainer wants one.
- When it lands: drop `firmware/vendor/`, move `[patch.crates-io]` back to a
  version bump, and delete the vendor README's "why" section into this card's
  log.

## Acceptance

`firmware/` builds against a published `hub75-framebuffer` with no vendored
copy, at the same brightness and bit depth.

## Log
