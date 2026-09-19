---
id: 061
title: A fixed-exposure capture path, so the camera can measure light
type: build
hardware: yes
depends: [007]
owner:
branch:
---

## Goal

Make `tools/cam-snap.sh` able to take a capture with **exposure, gain and white
balance locked**, so that two captures of the panel can be compared to each
other. Right now they cannot.

## Context

Found while doing card 007; the measurements are in that card's log.

The bench camera auto-exposes, and its range is enormous. A flat mid-grey field
displayed at 25 output-enable slots and at 2 — a 12.5x change in emitted light
— came back at camera code 123 and 100. So:

- **No absolute photometry.** Nothing about how much light the panel emits can
  be read off a capture.
- **No cross-capture comparison.** "Is the panel dimmer at brightness 24 than
  at 255" is not answerable from two stills, which is an awkward thing to be
  unable to answer on a card about brightness.
- Card 007 worked around it entirely with within-frame comparisons: split
  fields captured twice with the halves swapped, above-versus-below ratios for
  ghosting, adjacent-step gaps for bit depth. Those are good techniques and
  should survive this card, but they cost captures and cleverness.
- The camera's tone curve is also not sRGB — it lifts shadows noticeably — so
  even a locked exposure gives relative light only after a curve is fitted.

Card 012 (camera homography) will want this too: locating four corner pixels is
easier when the exposure is not chasing them.

## Questions to answer

1. Does avfoundation through ffmpeg expose manual exposure/gain/WB on the
   Anker PowerConf C200 at all? ffmpeg's avfoundation input is thin; the
   controls may only be reachable through AVFoundation itself (a tiny Swift or
   PyObjC helper) or not at all on a UVC device on macOS.
2. If not reachable: can we get repeatable exposure another way — a fixed
   bright reference card in frame that gives the AE a constant to lock onto, or
   simply a longer capture whose first frames are discarded after the AE
   settles on a *known* displayed pattern?
3. Can we fit the camera's transfer curve once, by displaying a known set of
   duty ratios (which are exact: duty is linear in light) and reading the
   codes? That turns the camera into a relative photometer even if the
   exposure still floats, as long as it is constant within one capture.

## Deliverables

- `tools/cam-snap.sh` grows a locked-exposure mode, or this card records
  clearly that it cannot be done and why.
- If (3) works: the fitted curve in `docs/research/061-camera-photometry.md`,
  with the displayed-duty-versus-code table behind it.
- Whatever lands, a paragraph in `docs/research/000-bench-notes.md` telling the
  next worker what a capture can and cannot be used to measure.

## Acceptance

Two captures of the same pattern at two panel brightnesses differ by
approximately the duty ratio, or the card says why that is impossible here.

## Log
