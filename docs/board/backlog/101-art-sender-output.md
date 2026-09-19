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

## Update from the orchestrator (2026-09-19): card 011 has merged - this is unblocked

Everything this card was waiting for is on `main`. Read `crates/screeny/README.md`
("Embedding", at the top), `crates/screeny/examples/art_output.rs` (your `Output` /
`WireFrame` shapes restated locally, with the impl: about 15 lines) and section 5 of
`docs/design/generative-art-brief.md`, which was rewritten for you. In short:

```rust
use screeny::{Link, LinkConfig, Pixels, Target};
let mut link = Link::open_deferred(target, LinkConfig::default());  // never fails
// in Output::send:
let sent = match &frame.indexed {
    Some((palette, indices)) => link.send(Pixels::indexed(palette, indices))?,
    None => link.send(Pixels::rgb(&frame.rgb))?,
};
```

- `Pixels` takes slices, so your `Vec`s go straight in. `impl From<screeny::Error> for
  std::io::Error` exists so the `?` works inside `io::Result`.
- Indexed frames are exact: <= 16 colours `PAL4_LZ`, <= 32 always (raw `PAL5` cannot
  overflow), 33-256 exact when the indices compress. `Sent::exact()`, `codec()`,
  `bytes()` tell you what happened per frame; they replace `budget.rs`'s estimates.
  A fallback counter belongs on the studio's stats strip.
- **Keep your 60 fps loop.** `Link` applies the device's cadence ceiling and reports
  `Sent::Coalesced` for frames it folded; `Limits` (`link.limits()`) exposes fps,
  budget, guaranteed-exact palette size and codecs for the connected device.
- The panel going away is not your problem: `Link::send` cannot fail because of the
  network; it reconnects on a background thread (re-resolving by mDNS name), and
  `link.state()` / `link.stats()` are there for a status light. Drop the link to send
  `FINAL` and release the panel.
- Two layout changes are coming that affect where you work, so **start from a fresh
  `main`**: (1) the WiFi-credential scrub rewrote history on 2026-09-19 - old clones
  and the old `claude/generative-art-designer-624186` branch must not be merged or
  pushed again, re-clone instead; (2) card 017 moves `art/screeny-art` ->
  `crates/art` and `art/studio` -> `crates/studio` in one workspace, and the plan in
  `docs/design/studio-vision.md` then drops Tauri for a server-first Studio (card 105).
  If 017 has not happened when you start, ask the orchestrator before building on the
  old layout.
- First milestone the owner wants: design a piece in the Studio and watch it on the
  real panel (192.168.7.221, mDNS instance `screeny-4a00a4`). Streaming over WiFi
  from this card is expected and fine; serial and flashing stay with the orchestrator.

## Log
