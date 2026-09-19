---
id: 082
title: sim - the squint view and the four-number content overlay
type: build
hardware: no
depends: [006]
owner:
branch:
---

## Goal

`docs/design/generative-art-brief.md` section 5 asks a faithful preview for
four things. Card 006 built three of them: the panel model, the LED dots, and
a statistics overlay. The fourth - "a *squint* view: the same thing blurred, to
approximate viewing distance" - and the brief's own four content numbers are
still missing, and they are the two that tell an artist whether a piece works
rather than whether the network does.

## Context

- The brief: "Judge every piece in that preview, at actual physical size on
  your screen if you can (the panel is about 19 x 10 cm), not zoomed to fill a
  monitor." The window is currently 14x with no way to ask for physical size.
- The four numbers the brief wants, none of which `screeny-sim` computes:
  distinct colours this frame; estimated encoded size against 1464 bytes;
  average picture level; maximum frame-to-frame luminance change.
- The encoded-size estimate needs an encoder, which lands with card 009. Until
  then the *actual* size of the frame that arrived is a good stand-in and is
  already on screen.
- `crates/sim/src/window.rs` has the dot stamp and the overlay; both would take
  this without restructuring.

## Deliverables

- `--squint` and a key to toggle it while running: a Gaussian blur of the dot
  render at roughly the angular size of the panel across a room.
- `--physical` or `--mm-per-pixel`, so the window can be sized to the real
  panel rather than to a round upscale factor.
- The four content numbers, computed from the decoded frame, on their own
  overlay line, with the frame-to-frame delta kept between frames.
- Unit tests for the four numbers against hand-built frames.

## Acceptance

Someone writing a piece for the panel can judge it in `screeny-sim` without
having to build their own preview, which is what the brief asks for.

## Log
