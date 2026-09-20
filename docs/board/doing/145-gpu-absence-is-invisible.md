---
id: 145
title: A missing GPU is only visible on stderr; the UI shows black and says nothing
type: build
hardware: no
depends: [106, 107]
owner: worker-173
branch: card/studio-page-tells-the-truth
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

### The card against today's tree

Still exactly as written. `Gpu::shared()` is still a `OnceLock` that keeps the
error, each GPU piece's `open()` still prints one line to stderr and returns
`None`, and nothing reaches the page. Card 170 changed which player renders,
not how a piece opens, so the black rectangle is the same black rectangle -
except that now it is *the panel's* picture, not a preview, so it matters more.

**Adapted to the one-page model:** the card said "where somebody will see it
when a GPU piece is selected". There is one page now and one piece list on it,
so that place is the piece list itself.

### What I did

- `crates/art/src/lib.rs`: `GpuStatus { available, adapter, backend, error }`
  and `gpu_status()`. It lives in `lib.rs`, not in `gpu/`, because it has to
  exist in a `--no-default-features` build too - where the honest answer is
  "this build has no GPU pieces" rather than a missing symbol.
- `crates/art/src/gpu/mod.rs`: `Gpu` remembers the adapter name and backend it
  opened with, and `gpu::status()` reports the cached outcome. Decided once,
  by the same `OnceLock`, and **no piece has to have been opened first**.
- `crates/art/src/pieces/mod.rs`: `NEEDS_GPU` / `needs_gpu(id)`, built from
  the same `#[cfg(feature = "gpu")]`s as `ALL`, so it cannot drift from it.
- `crates/studio`: `GET /api/v1/bootstrap` gains `gpu` and a `needs_gpu` flag
  per piece; `GET /api/v1/status` gains `gpu`. Both additive.
  `health.rs`'s doc comment now says in so many words that **a missing adapter
  is never a 503** - the CPU pieces are unaffected and no restart conjures a
  GPU.
- `main.rs`: one line on stdout at startup, either
  `studio: gpu Intel(R) Graphics (RPL-P) (Vulkan)` or
  `studio: no GPU adapter: ... - overland, lattice, knot cannot be played here`.
  Opening the device there is what the first GPU piece would have done anyway,
  and it keeps the first `/api/v1/status` prompt.
- The page: a piece that needs an adapter there is none for is **struck
  through, tagged "no GPU" and `disabled`** - not offered and then black - and
  the reason is one line under the list. If the piece that is *already loaded*
  is one of them (which is how a state file from a machine with a GPU arrives
  in a container without one) the stage carries a notice until another piece
  is picked; picking one clears it.
- Nothing about how a piece renders changed.

### Evidence

Forcing the no-adapter case on this Mac with `WGPU_BACKEND=vulkan` (wgpu is
built without the Vulkan backend here, so there is genuinely no adapter -
honest, and no Linux box needed):

```
$ WGPU_BACKEND=vulkan ./target/debug/screeny-studio --listen 127.0.0.1:8791 \
    --no-discover --state-dir <tmp> --ui-dir crates/studio/ui
studio: no GPU adapter: No suitable graphics adapter found; noop not requested,
  vulkan support not compiled in, metal not requested, dx12 not requested, gl not
  requested, webgpu not requested - overland, lattice, knot cannot be played here

$ curl -s localhost:8791/api/v1/status | jq '{gpu, ok}'
{ "gpu": { "available": false, "adapter": "", "backend": "",
           "error": "no GPU adapter: No suitable graphics adapter found; ..." },
  "ok": true }
```

`ok` is `true` and `/healthz` is 200, which is the point.

Tests: `tests/ui.rs::the_gpu_outcome_is_on_the_api_and_is_never_a_fault` (the
two routes cannot disagree, the marked pieces are exactly `NEEDS_GPU`, the
answer is never a 503, and whichever way the machine went the matching half is
filled in) and `the_page_says_why_a_gpu_piece_is_not_available`.

`docs/design/deployment.md` verification (b) rewritten: it no longer has to
tell the operator to select a piece and grep `docker logs`, because the line is
at startup and the fact is on `/api/v1/status` and on the page.
