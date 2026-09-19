---
id: 020
title: Panel brightness and current limiting without losing colour depth
type: research
hardware: no
depends: [001]
owner: claude-fable-5.1 (answered inside card 007)
branch: card/007b-firmware-display
---

## Goal

Find a way to run the panel at a safe brightness that does not cost us most of
the colour depth we have. Produce a recommendation the firmware can implement
and card 002 can encode against.

## Context

Found while doing card 001; see `docs/research/001-firmware-stack.md` sections
3 and 9.

Tidbyt dims the panel by shortening the output-enable window: their
`setBrightness8(n)` lights `n * 64 / 256` of the 64 columns in each row. Their
shipping default is 30/255, about 12% duty, and their maximum is 100/255,
about 39%. Every pixel value keeps its full range; the whole panel just spends
less time lit.

`esp-hub75` has no equivalent. It has no output-enable duty control at all.
The levers it does offer are `trail-blank-N` and `inter-row-blank-N`, which
blank a handful of pixel clocks around the row address change — enough to
stop ghosting, nowhere near enough to reach 12% duty.

So the skeleton dims by scaling pixel values before writing them to the
framebuffer. That works, and draws roughly the right average current, but it
is the expensive way to do it: at a 30/255 cap, a 6-bit BCM channel only ever
uses values 0-7. **We lose about three of our six bits exactly where we have
the least to spare**, and card 002 is trying to fit a good picture into
5-6 bits in the first place.

Two things make this worth real thought rather than a shrug:

- The panel runs off laptop USB on this bench, so the current limit is not
  optional.
- Nobody has measured what this panel actually draws. No source states a
  figure, so the 39% cap may be conservative, or may not be.

## Questions to answer

1. **What does the panel actually draw?** At full white and at a few
   brightness settings, on this hardware. This is the number everything else
   depends on and it does not exist yet. Needs a bench measurement and
   therefore probably a `hardware: yes` follow-up card — say so if it does.
2. **Can we get output-enable duty control out of `esp-hub75`?** Options,
   roughly in order of preference:
   - a feature or config it already has that card 001 missed;
   - `inter-row-blank-32` plus a reduced column count, or some combination
     that reaches a low duty cycle;
   - a patch upstream. `liebman/esp-hub75` is actively maintained and the
     author asks for contributions; a `Hub75Config::brightness` that shortens
     the OE window would be useful to everyone with a USB-powered panel, not
     just us. Estimate the work.
   - the `i2s_parallel_dimming` example, which uses a delay as global
     brightness control — but note it appears to rely on the per-plane
     interrupt path, which `circular-dma` removes. Check whether the two can
     coexist.
3. **How bad is value-scaling really?** Not in bits, in pictures. Render the
   same test content at 6 planes scaled to 30/255 and at 6 planes unscaled,
   and look at them. Gamma matters here: LED response is roughly linear, so a
   gamma-corrected ramp spends most of its codes at the bottom of the range
   anyway, which may mean the loss is smaller than the bit count suggests.
4. **Is there a hybrid?** For example, cap with a modest blanking increase
   (cheap, depth-preserving) and make up the rest with value scaling, so the
   scaling factor is 1/2 rather than 1/8.
5. **Does temporal dithering across panel refreshes buy depth back?** The
   panel refreshes at 154 Hz against a 30 fps stream, so there are about five
   refreshes per frame. Alternating between adjacent BCM values across those
   refreshes would recover a bit or two. Is that reachable through
   `esp-hub75`'s swap API, and what does it cost in CPU?
6. **What should the firmware's brightness control look like?** A runtime
   setting over the control channel (card 003), a compile-time cap, or both?
   What is the hard maximum we will never exceed, and where is it enforced?

## Deliverables

- `docs/research/020-panel-brightness.md`: conclusions first. The
  recommendation, what it costs in colour depth, and the numbers behind it.
- If the answer is an upstream patch, a concrete description of it and an
  estimate — do not write it under this card.
- If a bench measurement is needed, a `hardware: yes` card in `backlog/`
  describing exactly what to measure and how.
- A note for card 002 stating the effective colour depth it should design
  against.

## Acceptance

Card 007 knows what brightness to ship and why, and card 002 knows how many
bits per channel it is really encoding for.

## Log

### Answered inside card 007, on hardware (2026-09-19)

**The premise of this card is wrong, and that is the answer.** `esp-hub75` does
not need to grow output-enable duty control: `hub75-framebuffer` already has
the mechanism, it is simply not exposed. The output-enable line is **bit 8 of
every 16-bit framebuffer entry**, written once by `make_data_template()` when
the buffer is formatted and touched by nothing else — `set_pixel` and `erase`
both mask bits 9..14 only. So the lit window inside each 64-slot scan row can
be rewritten at run time, per buffer, without disturbing colour data, the row
address or the latch.

That is exactly Tidbyt's `setBrightness8()`. It is implemented in
`firmware/vendor/hub75-framebuffer` (see `firmware/vendor/README.md`) as
`set_oe_slots(lit)` / `set_oe_window(start, lit)`, about 40 lines.

**Taking the questions in order.**

1. *What does the panel actually draw?* Still unmeasured, and this card no
   longer depends on it. We ship Tidbyt's own documented maximum — 25 of 64
   slots, 39% duty, their "brightness 100" — as a hard cap in
   `display::MAX_OE_SLOTS`, and come up at 9 slots (14%), just above their
   shipping default. Since the mechanism is now the same one Tidbyt uses, their
   numbers transfer directly, which they did not when we were scaling pixel
   values. No ammeter card raised; raise one if the cap ever needs lifting.
2. *Can we get OE duty out of `esp-hub75`?* Yes — option "a feature or config
   it already has that card 001 missed", near enough. Not upstream yet; card
   060 covers offering it.
3. *How bad is value-scaling really?* Moot, and never measured, because
   nothing scales values any more.
4. *Is there a hybrid?* Not needed. The duty control reaches 0-39% on its own.
5. *Does temporal dithering buy depth back?* Yes, and card 007 took it as its
   stretch. It is reachable through the swap API, it costs about 40 points of
   one core (the framebuffer must be rewritten every refresh rather than every
   frame), and it moves the black point from sRGB 34 to about sRGB 6. See card
   030's log.
6. *What should the control look like?* Runtime 0..=255 over the control
   channel, with the cap enforced in firmware at `display::MAX_OE_SLOTS` and
   nowhere else. One bench-only escape hatch exists (`testcmd`'s `O` command,
   raw slots, self-reverting after 10 s) so a session can photograph a duty we
   would never ship.

**What it costs in colour depth: nothing.** Every BCM plane keeps its weight
and its range; the panel simply spends less time lit. Measured: a 16-step grey
wedge at 25 slots and at 2 slots has all sixteen steps monotonic and separated
at both. The gamma table does not move by one entry between them.

**The note for card 002:** design for **six bits per channel**, not three.
Effective depth is six planes, unreduced by brightness, and with the temporal
dither on it behaves like roughly 7.6 — which is the figure card 002's own
panel model already assumed for `BC1_DUAL`. The "brightness cap eats more"
caveat in card 001 section 3 no longer applies and should be read as withdrawn.

**One real limitation** to carry forward: the control's resolution is 25 steps,
not 256, because the panel is lit for a whole number of pixel clocks or not at
all. `display::slots_for` rounds; below brightness 6 the panel is dark.

No `docs/research/020-panel-brightness.md` was written: the conclusions are
short, they are already in `firmware/vendor/README.md` (the mechanism) and card
007's log (the measurements), and a third copy would be the one that goes
stale. Moving to `review/`.
