---
id: 161
title: Studio UI - at a narrow window the preview overlaps the inspector and meters spill
type: build
hardware: no
depends: [105]
owner:
branch:
---

## Goal

The Studio's design view is usable in a narrow browser window and on a phone.

## Context

First real render of the card-105 UI, the owner's screenshot of 2026-09-19 (a ~600 px wide
Chrome window on `http://workbench.local:8787/`): the wooden-frame panel preview is drawn
on top of the inspector column (it covers the "Choreography" parameter's label and slider),
the seed readout is clipped ("SEE", "25"), and tick marks from the Colours / Frame size
meters appear inside the parameters column beside "Dial rings". At a wide window the layout
is presumably as designed (it was built for a Tauri window of fixed minimum size).

Card 106 adds a dashboard meant to work from a phone; this card is the same standard for
the design view. Related: card 121 (nothing checks the front end), card 120 (preview
bandwidth).

## Deliverables

- A breakpoint below which the layout stacks (preview on top, inspector below), with the
  preview never overlapping controls and meters contained in their cells.
- Static files only; no Node toolchain, no CDN. Keep the design language.
- Checked at 390, 600, 900 and 1400 px wide, in a real browser, with screenshots in the Log.

## Acceptance

No overlapping or clipped controls at any of those widths; the owner can change a piece and
its parameters from his phone.

## Log
