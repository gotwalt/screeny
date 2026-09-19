---
id: 070
title: Deeper fractal zoom without paying for it in iterations
type: build
hardware: no
depends: [010]
status: parked
---

## Goal

Let a `crates/demos` fractal leg zoom much further than the ten octaves card 010
settled on, without the frame time growing with depth.

## Context

Card 010 measured what depth actually costs (`preview probe`): escape counts near
the boundary grow roughly geometrically with zoom, so a twelve-octave view needs
~5000 iterations for its 90th-percentile pixel and a twenty-octave view ~50000.
At 9-16 samples per pixel that walks off the 33 ms budget somewhere around ten
octaves, which is where the tour's legs now stop and cross-dissolve.

Ten octaves looks good and nothing in the current piece is visibly limited by it -
this card is not fixing a defect. It is worth doing only if someone wants a leg
that keeps going for ten minutes instead of ninety seconds.

The standard answers, in increasing order of work:

- **Distance-estimate bail-out.** Carry the derivative `dz` alongside `z`; when
  `|dz|` exceeds roughly `4 |z| log|z| / pixel_size` the point is inside a pixel
  of the boundary and its exact escape count no longer matters, so stop and shade
  it as boundary. Doubles the per-iteration cost, but cuts the pixels that run to
  the ceiling from thousands of iterations to hundreds. Also gives distance-based
  shading for free, which is a different and rather good look on this panel.
- **Perturbation plus series approximation.** One high-precision reference orbit,
  everything else as a low-precision delta. This is how deep-zoom software gets to
  1e-300. It needs big-float arithmetic for the reference (a dependency, or a
  hand-rolled fixed-point) and glitch detection.

## Deliverables

- Whichever of the two above earns its keep, behind the existing `FractalZoom`
  parameters, with `preview probe` numbers before and after.
- A measured ms/frame at the new maximum depth.

## Acceptance

A leg reaches at least twenty octaves with frame times no worse than today's
ten-octave numbers, and the previews at depth look at least as good.

## Log
