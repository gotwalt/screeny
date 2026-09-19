---
id: 100
title: Generative art system (art/): pipeline, studio, and first pieces
type: build
hardware: no
depends: []
owner: Claude (generative art designer session), with the owner
branch: claude/generative-art-designer-624186
---

This card is written after the fact. The work was done directly with the owner in a
design session rather than from the board, so this is the hand-over for review.

## Goal

A system for making generative art for the panel, following
`docs/design/generative-art-brief.md`: a headless core that renders pieces through a
panel-aware pipeline, and a desktop studio for designing them. Eventually it runs
headless on a Linux box with a GPU and is the primary sender (card 011).

## What is in the branch

Everything is under `art/`, its own Cargo workspace (main already excludes it from the
root workspace). Nothing outside `art/` changes except this card, its follow-ups and
`.gitignore`. `art/README.md` is the real documentation; in brief:

- `art/screeny-art`: library + `screeny-art` binary (`list`, `pipe`, `snapshot`).
  Pieces -> limiter (APL cap, rise limiter) -> quantise to panel levels with fixed
  ordered dither -> `WireFrame` -> `Output`. Linear light throughout, OKLCH palettes,
  own seeded PRNG. GPU pieces render headless through wgpu (Metal here; Vulkan/GLES
  on Linux, untested). Pieces: two clock pieces after ClockClock 24 (`clocks-numerals`,
  `clocks-dials`) with motor-limited choreography, a dance composer and a day-long
  variety test; `overland` (procedural 3D world painted by palette index); `plasma`,
  `metaballs`, `lattice`, `knot`, `testcard`.
- `art/studio`: Tauri v2 app, static front end (no Node). Draws the pipeline's output
  as LEDs, with the brief's four meters.
- It never talks to the device, the serial port or the camera. Frames leave through
  `screeny_art::output::Output` (`fn send(&mut self, &WireFrame) -> io::Result<()>`).
  A `WireFrame` always has `rgb` (6144 sRGB bytes) and, when the piece rendered
  indexed, `indexed: Some((palette: Vec<[u8; 3]>, indices: Vec<u8>))` with at most 32
  colours. That is the shape card 011 was written against.

## State

- Merged `main` into the branch at 364a035 to make the merge back clean. One
  conflict, `.gitignore` (both sides added `art/studio/gen/`); took main's.
- In the merged tree: `cd art && cargo test -p screeny-art` passes 31 tests;
  `cargo build --workspace` builds; the root workspace resolves without `art`.
- `cargo clippy` in `art/`: no errors; a few style warnings left.
- Verified by tests and rendered stills. The studio UI was checked in a browser
  against a mock engine, because the agent shell cannot capture the Tauri window;
  the owner has been running the real app throughout.

## What the orchestrator should know

1. **It assumes 60 fps**, on the owner's instruction ("let's assume we can actually
   do 60fps on this device"). The studio engine and `pipe` default to 60, 30
   selectable. Card 011 item 5 (fps as the caller's choice) fits this.
2. **The panel model in `art/` is now behind the device.** It was built from the
   first brief. Since then: device-side temporal dithering (darks far better than a
   hard 64-level quantise), brightness that no longer costs depth, real codecs with
   `PAL8_LZ` making up to 256 colours exact when the index image compresses. The
   preview is therefore pessimistic, and the "stay within 32 colours" rule the pieces
   follow is stricter than it needs to be. See card 102. Nothing here is wrong on the
   wire: frames are valid sRGB/indexed frames either way.
3. `art/README.md` has a table, "Provisional assumptions", naming the single place
   each assumption about the device lives. That is the reconciliation checklist.
4. No files are written outside the repo. (A ratings feature briefly wrote
   `~/.screeny-art/clocks.taste`; it was removed at the owner's request.)
5. Dependencies are heavy (Tauri, wgpu) but confined to `art/Cargo.lock`.
   `cargo build -p screeny-art --no-default-features` drops wgpu and the GPU pieces.

## Acceptance (proposed)

- Merges to `main` with no changes outside `art/`, `docs/board/` and `.gitignore`.
- `cd art && cargo test -p screeny-art` passes; `cargo run -p screeny-studio` opens.
- Root workspace unaffected: `cargo test` at the root behaves as before.

## Follow-ups written as cards

101 (send through `crates/screeny`), 102 (reconcile the panel model and colour
budget with the measured device), 103 (headless Linux GPU run), 104 (runner that
rotates pieces).

## Log

- 2026-09-19: built in one session with the owner. Commits 099abee..4666b68 on the
  branch, then the merge of main (364a035) and this card.
