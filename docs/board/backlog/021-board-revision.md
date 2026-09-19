---
id: 021
title: Detect the Gen 1 board revision and its RGB channel order
type: build
hardware: yes
depends: [001, 007]
owner:
branch:
---

## Goal

Make the firmware work on whatever Gen 1 revision this particular Tidbyt is,
rather than on the one revision Tidbyt's HDK happens to target.

## Context

Found while doing card 001; see `docs/research/001-firmware-stack.md`
section 2.

The HDK's pin map is a single fixed table, but **the stock firmware is not**.
Its log format string is

```
Display for rev %d, RGB=%d,%d,%d
```

under the tag `tidbyt/display` — so the factory firmware chooses its RGB pin
assignment at run time from a board revision it detects. It detects that
revision by ADC-reading two straps, GPIO13 and GPIO15
(`Couldn't adc read IO13: %s`, `Hardware generation: gen%d, %dmV, %dmV`,
`Hardware rev 0x%02x`).

The community has hit the same thing from the other side: the tronbyt
firmware ships two Gen 1 build targets, `tidbyt-gen1` and `tidbyt-gen1_swap`,
differing only in a rotation of the colour channels (R -> B -> G -> R).

So `spike/fw-skeleton`'s pin map may simply produce the wrong colours on this
unit. That is easy to spot — the test pattern draws red, green and blue bands
top to bottom for exactly this reason — and easy to work around by hand, but
we should not leave a hand-edit in the source.

## Questions to answer

1. What do the two ADC straps read on this unit, and what generation and
   revision do they encode? The stock firmware's mapping from millivolts to
   revision is not published; recover what we can by measurement, and say
   plainly what stays unknown.
2. Which RGB order does this unit actually need? Determined by eye, or by the
   camera rig from card 012 if it is ready.
3. Is a runtime table worth it, or is a compile-time constant honest? We own
   exactly one device. A detected-at-boot table that has only ever been tested
   against one reading is not more correct than a constant, just harder to
   read — but it does log the strap voltages, which is worth having.

## Deliverables

- Firmware reads both straps at boot and logs the raw millivolts, the derived
  generation and revision, and the RGB order it chose.
- The RGB order is a named configuration rather than an edit to the pin
  initialiser, so changing it does not mean rewriting `Hub75Pins16`.
- `docs/research/021-board-revision.md`: what this unit reads, what that
  means, and what is still guesswork.
- Update `spike/fw-skeleton/src/tidbyt.rs` (or its successor in `firmware/`)
  with whatever is now known.

## Acceptance

The firmware shows correct colours on this Tidbyt without anyone editing a pin
map, and the boot log says enough that a second unit could be diagnosed from
it.

## Log

### What card 007 learned (not taken; stays in backlog)

- The rotated colour order is **confirmed a second time on hardware**. Card
  007's orientation and ghost patterns draw red, green, blue, yellow and white
  at known positions and all of them come out in the right channel with the
  current mapping (`red=GPIO2/4, green=GPIO22/27, blue=GPIO21/23`). So the
  *constant* is right for this unit and nothing is urgent here.
- The hand-edit this card objects to has moved but not gone: it is now in
  `firmware/src/main.rs`'s `Hub75Pins16` initialiser, with a comment
  explaining it and pointing here. The named-configuration deliverable is
  untouched.
- The two ADC straps (GPIO13, GPIO15) have still never been read. Card 007 had
  hardware access and did not spend it on this, on purpose: it is a separate
  question from the display pipeline and it wants its own careful boot-log
  work.
- Useful when this card is picked up: `firmware/src/patterns.rs` now has a
  ready-made `Bands` pattern (red, green and blue bands, each ramping) and an
  `Orientation` card, selectable at run time over UDP through
  `firmware/src/testcmd.rs` without reflashing. Checking a candidate RGB order
  is one datagram, not a rebuild.
- Nothing card 007 did blocks this card, and nothing in it depends on card 007
  beyond `firmware/` now existing.
