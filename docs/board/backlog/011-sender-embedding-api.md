---
id: 011
title: Make crates/screeny a good library to embed (the art system becomes the primary sender)
type: build
hardware: no
depends: [009]
owner:
branch:
---

## Goal

The generative art system (`art/`, developed by another Claude instance on branch
`claude/generative-art-designer-624186`, not yet merged) will become the primary
sender to the real panel. It should link `crates/screeny` as a library and push frames
through it, exactly, with no CLI in between. Make that a ten-line job with no sharp
edges, on macOS now and headless Linux later.

## Context

- The art pipeline (read-only reference: `git show
  claude/generative-art-designer-624186:art/README.md`, and
  `...:art/screeny-art/src/output.rs`, `frame.rs`, `pipeline.rs`) ends in a `WireFrame`
  that is either **indexed** (palette of up to 32 sRGB colours + 2048 indices; its
  preferred, exact form) or RGB. Frames leave through its `Output` trait
  (`fn send(&mut self, ...)`-style, called by its own paced loop). Today its only
  output is raw RGB on stdout. It owns its own pacing, limiter and clock, and has
  started rendering at 60 fps.
- Do NOT modify anything under `art/` or that branch. This card shapes *our* library
  so that their `Output` impl is trivial; they will write it after merging.
- Gaps found by the orchestrator:
  1. `Sender::send_frame` takes only `&Rgb888Frame`. There is no way to send an
     indexed frame through the `Sender`, although `Encoder::encode_indexed` exists. An
     indexed frame must go on the wire exactly (`PAL4_LZ` / `PAL5` / `PAL8_LZ`,
     never re-quantised), and must never exceed the budget (fall back to the lossy RGB
     path only if an exact encoding genuinely cannot fit, and say so in stats).
  2. The push model is under-served: `run`/`run_with` own the loop and pull from a
     `FrameSource`. An embedder that owns its own loop needs a clear push API: connect,
     `send_frame` / `send_indexed` whenever it likes, the sender handles seq,
     `STATS_REQ` cadence, telemetry intake, adaptation hints, and `FINAL` on drop.
     Decide and document what happens if the caller pushes faster than the device
     advertises/handles, and whether pacing is the caller's job (it should be able to be).
  3. Reconnect behaviour for a long-running daemon: device reboots, WiFi drops, DHCP
     address changes. A sender that runs for weeks must recover by itself (re-resolve
     by mDNS name, new socket per spec 3.2, backoff) and keep accepting frames
     (dropping them) meanwhile instead of returning errors the embedder must handle.
  4. Linux: `crates/screeny` has `cfg(target_os = "linux")` socket paths that have
     never been compiled. `rustup target add x86_64-unknown-linux-gnu` and
     `aarch64-unknown-linux-gnu`, then `cargo check -p screeny --target ...` for both
     (check needs no linker). Fix what breaks. mDNS on headless Linux: note any
     requirements (`mdns-sd` is pure Rust; confirm no avahi dependency).
  5. Frame rate: the firmware was measured clean up to 120 fps on the bench (card
     008), and the owner's target remains 30. Make fps a caller choice with a sane
     default, and make `GET_INFO`/telemetry-driven limits visible to the embedder.
- `FrameSource` could also gain an indexed variant so the built-in demos (word clock
  is <= 8 colours) go out exact without an RGB round trip. Optional; do it if cheap.

## Deliverables

- `Sender::send_indexed` (or an enum frame type accepted by one `send`), exactness
  tested end to end against `crates/sim`'s `SimDevice` (add `screeny-sim` as a
  dev-dependency): the simulator's decoded frame must equal palette[index] for every
  pixel, for palettes of 2, 16, 17, 32 colours and for incompressible index noise.
- A documented push API + auto-reconnect, with tests against `SimDevice` (kill and
  restart the sim mid-stream on the same and on a different port/name).
- `crates/screeny/examples/embed.rs`: the ten-line embedding, plus
  `examples/art_output.rs`: a self-contained sketch of what the art system's `Output`
  impl will look like (define a stand-in trait locally with the same shape as theirs).
- Linux `cargo check` clean for both targets; note results in the log.
- `crates/screeny/README.md`: an "Embedding" section at the top. Public API reviewed
  for naming and doc comments; `#![warn(missing_docs)]` on the library if practical.
- `docs/design/generative-art-brief.md` section 5 ("How your system should hand over
  frames") rewritten to describe the real API. Leave the other sections alone; the
  orchestrator is updating them.

## Rules for this card

- No hardware access; do not send packets to the real device. Use `SimDevice`.
- Every test bounded and fast; no test over ~10 s; no background processes left
  behind when you finish (check with `pgrep -lf 'screeny'`).
- Append to the Log after each deliverable, and commit after each.

## Acceptance

`cargo test -p screeny` passes including the new sim-backed tests; both Linux targets
check clean; an embedder can stream exact indexed frames with auto-reconnect using
only what the README's Embedding section shows.

## Log
