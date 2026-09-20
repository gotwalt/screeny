---
id: 135
title: The simulator can only dump what was decoded, never what the panel shows
type: build
hardware: no
depends: [066]
---

## Goal

Give `screeny-sim` a headless way to write out the frame *after* the panel
model, so brightness, quantisation and the idle screens can be judged without a
window and without a camera.

## Context - found while doing card 066

`--dump-dir` is fed `Snapshot::decoded`, and `crates/sim/src/dump.rs` says so
deliberately: "a dump is for diffing against what a sender thought it encoded,
so it has to be the bytes the decoder produced and nothing else. The LED-dot
view is for looking at; this is for `cmp`." That is right, and it should stay
the default.

But it means the only way to see the simulator's *picture* is to open a window.
Card 066 changed the brightness model and could produce no dumped image of the
change - the PNGs are byte-identical at every brightness, because brightness is
not in them. With the camera disconnected (CLAUDE.md, 2026-09-19) and card 061
parked, the LED-dot window is the only judge of colour we have, and a worker
on a headless box cannot reach it.

## Deliverables

- A second sink, e.g. `--dump-panel-dir PATH` (or `--dump-stage panel`), that
  writes the post-panel-model frame - `Snapshot::panel`, the same buffer the
  window draws - through the same `Dumper`. The existing flag keeps its
  meaning and its doc comment.
- The `--help` text says which of the two is bit-exact and which is the
  picture, in one line each.
- A test that a panel dump at brightness 255 and one at 128 differ, and that a
  decoded dump at the two brightnesses does not.

## Acceptance

A headless run can produce a PNG that shows what the panel would look like, and
the existing `--dump-dir` bytes are unchanged.
