---
id: 145
title: A missing GPU is only visible on stderr; the UI shows black and says nothing
type: build
hardware: no
depends: [106, 107]
---

## Goal

When there is no usable graphics adapter, the studio says so where somebody will see
it, instead of quietly showing a black panel.

## Context

Found while containerising the studio (card 107). `crates/art/src/gpu/mod.rs`
`Gpu::shared()` opens the device once and keeps the error, and each GPU piece's
`open()` does:

```rust
Err(e) => { eprintln!("screeny-art: {}: {e}; rendering black", self.label); return None; }
```

That is the right *behaviour* - one line, not a crash, not a loop - but the only
place it appears is the process's stderr. In a container that is `docker logs`, which
nobody is reading. A browser on `workbench.local:8787` picks `overland`, sees a black
64x32 rectangle, and has nothing to go on.

`docs/design/studio-vision.md` already promises the opposite: "fall back to CPU pieces
if no adapter is found **and say so in the UI**".

Deployment note that depends on this: `docs/design/deployment.md`, verification (b),
currently has to tell the operator to grep `docker logs` for
`screeny-art: gpu=<name> backend=<backend>`.

## Deliverables

- The adapter outcome is part of the studio's state, not just a log line: the adapter
  name and backend when there is one, the error string when there is not. It is
  decided once (`Gpu::shared()` is already a `OnceLock`) and does not need a piece to
  have been opened first.
- `GET /api/v1/status` (card 106) carries it, and `GET /api/v1/bootstrap` marks which
  pieces need a GPU.
- The UI says it once, plainly, where it cannot be missed when a GPU piece is
  selected - and marks GPU pieces as unavailable rather than letting them be picked
  and render black.
- The same fact on stdout at startup, once, so `docker logs` answers it without
  anyone having to select a piece first.

## Acceptance

Run the studio on a machine with no adapter (or in a container with no `/dev/dri` and
`SCREENY_FEATURES=none`), open the UI, and the reason the GPU pieces are not
available is on the screen. `curl /api/v1/status` says the same thing.

## Log
