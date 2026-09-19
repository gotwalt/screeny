---
id: 101
title: art/ sends to the panel through crates/screeny
type: build
hardware: no
depends: [011, 100]
owner:
branch:
---

## Goal

Give `art/screeny-art` an `Output` that pushes frames through `crates/screeny`, so the
art system is a real sender: indexed frames exact, RGB frames through the sender's
encoder. Replace the stand-in byte-budget estimates with the real encoder's answer.

## Context

- `art/screeny-art/src/output.rs` (`Output`, `PipeOutput`), `frame.rs` (`WireFrame`).
- Card 011 shapes `crates/screeny` for exactly this and sketches the impl in
  `crates/screeny/examples/art_output.rs`. Start from that.
- `art/screeny-art/src/budget.rs` estimates encoded size and fakes a lossy encode for
  the studio preview. With the real encoder available it should report the real codec
  chosen and real byte count, and the preview should show the real decoded frame.
  (This is what the deleted card 071 asked for.)

## Deliverables

- `art/screeny-art/src/output/sender.rs` (or similar): `SenderOutput`, behind a cargo
  feature so the core still builds with no network stack.
- `screeny-art play <piece> --to <name-or-addr>`; the studio gains a "send to panel"
  switch that drives the same output alongside the preview.
- `budget.rs` replaced by calls into `screeny`'s encoder; studio meters show the real
  codec and size.

## Acceptance

- Against `crates/sim`: an indexed piece (`clocks-numerals`, `overland`) arrives
  pixel-exact; a continuous piece arrives and the studio preview matches the sim.
- Only the orchestrator (or a `hardware: yes` card) points it at the real panel.

## Log
