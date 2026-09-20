---
id: 101
title: The art system sends to the panel through crates/screeny
type: build
hardware: no
depends: [011, 100]
owner: worker-101
branch: card/101-art-sender-output
---

## Goal

Give `crates/art` an `Output` that pushes frames through `crates/screeny`, so the
art system is a real sender: indexed frames exact, RGB frames through the sender's
encoder. Replace the stand-in byte-budget estimates with the real encoder's answer.

## Context

- `crates/art/src/output.rs` (`Output`, `PipeOutput`), `frame.rs` (`WireFrame`).
- Card 011 shapes `crates/screeny` for exactly this and sketches the impl in
  `crates/screeny/examples/art_output.rs`. Start from that.
- `crates/art/src/budget.rs` estimates encoded size and fakes a lossy encode for
  the studio preview. With the real encoder available it should report the real codec
  chosen and real byte count, and the preview should show the real decoded frame.
  (This is what the deleted card 071 asked for.)

## Deliverables

- `crates/art/src/output/sender.rs` (or similar): `SenderOutput`, behind a cargo
  feature so the core still builds with no network stack.
- `screeny-art play <piece> --to <name-or-addr>`; the studio gains a "send to panel"
  switch that drives the same output alongside the preview.
- `budget.rs` replaced by calls into `screeny`'s encoder; studio meters show the real
  codec and size.

## Acceptance

- Against `crates/sim`: an indexed piece (`clocks-numerals`, `overland`) arrives
  pixel-exact; a continuous piece arrives and the studio preview matches the sim.
- On the real panel (`screeny-4a00a4`, 192.168.7.221): `screeny-art play <piece> --to
  screeny-4a00a4` streams an indexed piece and a continuous piece; link state
  connected, frames accepted by the device, exact/fallback counts recorded. The owner
  judges the picture by eye.
- The sim acceptance passes first; the panel run is one bounded step at the end.
  WiFi streaming and read-only control queries only - never the serial port, flashing,
  the camera, `reboot` or `brightness`.

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
  `main`**: (1) the WiFi-credential scrub rewrote all history on 2026-09-19 - every commit hash
  changed, the old `claude/generative-art-designer-624186` branch and its worktree
  are gone (fully merged first), and any old clone or bundle must never be merged or
  pushed; branch from the current `main`. Credentials now come from
  `~/.config/screeny/wifi.env` via `firmware/build.rs` - never put real ones in a
  tracked file, test fixture or log; (2) card 017 has moved the art system into the single workspace - it now lives in
  `crates/art` (package `screeny-art`) and `crates/studio` - and the plan in
  `docs/design/studio-vision.md` then drops Tauri for a server-first Studio (card 105).
- First milestone the owner wants: design a piece in the Studio and watch it on the
  real panel (192.168.7.221, mDNS instance `screeny-4a00a4`). Streaming over WiFi
  from this card is expected and fine; serial and flashing stay with the orchestrator.

## Log

### 2026-09-19 - acceptance widened to the real panel (owner)

The owner decided this workstream is only accepted when it runs on the real panel, so
the orchestrator lifted the "sim only" rule for **one bounded step at the end**: after
the simulator acceptance is green, stream one indexed piece and one continuous piece to
`screeny-4a00a4` by mDNS name with `screeny-art play`, about 60 s each, under a
timeout, letting the link drop cleanly so `FINAL` releases the panel. The orchestrator
confirmed the device is up and discoverable (firmware 0.2.0, codecs pal8-lz / pal4-lz /
bc1-dual / pal5 / solid, mtu 1464) and is staying off it meanwhile. Serial, flashing,
the camera, `reboot` and `brightness` stay forbidden; control queries are read-only
(`info`, `stats`, `ping`). Acceptance section updated above.

### Step 1 - the real encoder replaces the estimates; `SenderOutput` exists

`budget.rs` is deleted. `meter.rs` replaces it and is not an estimate of anything: it
runs `screeny-encode`'s chooser (the code the sender runs) and then `screeny-proto`'s
decoder (the code the *firmware* runs), so `Measured { codec, bytes, exact, colours }`
and `Meter::decoded()` are the real codec, the real datagram size, the real exactness
decision and the real picture the panel will put up. `Encoding::for_colours` and
`simulate_lossy` (median cut + ordered dither, a stand-in for "what a lossy codec might
do") are gone with it.

Both crates are pure codec crates - `screeny-proto` has no dependencies at all - so
`crates/art` depends on them **unconditionally** and the studio's meters and preview are
honest with no panel and no network stack anywhere. Only `screeny` itself (sockets,
mDNS, `Link`) is optional, behind the new `sender` feature.

- `crates/art/src/meter.rs`: `Meter`, `Measured`, `PAYLOAD_BYTES` (now
  `screeny_proto::MAX_PIXEL_PAYLOAD`, which is the same 1464 the brief quotes, but from
  the spec rather than from a comment), `distinct_colours` kept.
- `crates/art/src/pipeline.rs`: `Pipeline` owns a `Meter`. `Stats.encoding: Encoding`
  becomes `codec: u8` + `exact: bool`, `encoded_bytes` is now measured rather than
  looked up, `Output` gains `measured`, and `Settings.lossy_sim` becomes
  `codec_preview` (serde `alias` so an old saved settings blob still loads). The
  preview is the decoded datagram when it is on.
- `crates/art/src/output/` is now a directory: `mod.rs` (the trait, `PipeOutput`) and
  `sender.rs` (`SenderOutput`, `PanelStatus`, `target_for`), the latter `#[cfg(feature
  = "sender")]`.
- `screeny-art play <piece> --to NAME|ADDR [--fps] [--seconds] [--wait] ...`, with a
  ctrl-c handler so `FINAL` goes out on a signal (`Drop` does not run on one).

The meter is stateful because the chooser's hysteresis is: it gives the previous
frame's codec an 8% advantage, so a meter fed a whole stream answers the same as a
sender fed the same stream, and a meter fed a *different subset* can differ on a
marginal frame. That matters for the preview-vs-sim comparison and is why the test
below sends 1:1.

Measured, not assumed, in `meter.rs`'s own tests: palettes of 2/16/17/32 colours are
exact and decode to themselves; 32 colours of pure index noise take `pal5` at exactly
1376 bytes and are still exact; 64 flat bands take `pal8-lz` and are still exact; a
continuous frame is not exact and the decode really differs from the framebuffer. Two
first attempts at test expectations were wrong and are worth recording: a 32-colour
frame with *structured* indices is exact even at a 600-byte budget (`pal8-lz`
compresses it), and a lossy continuous frame does **not** always land inside 256
colours, because `bc1-dual` is a block codec rather than a palette one.

`cargo build -p screeny-art --no-default-features` and `--features sender` both clean;
`cargo test --release -p screeny-art --features sender` 37 passed.

### Step 2 - the simulator acceptance, and one environmental finding

`crates/art/tests/sender.rs` (3 tests, all green first time, 2.0 s):

| test | what it pins |
|---|---|
| `an_indexed_piece_arrives_pixel_exact` | `clocks-numerals` and `plasma`: every displayed pixel is `palette[index]`, expanded independently of the pipeline, and the preview equals it |
| `a_continuous_piece_matches_the_preview` | `metaballs`: the device's decoded frame **is** the preview, byte for byte, and its codec and size are the ones the meter reported |
| `the_meter_agrees_with_the_link` | codec, bytes and exactness identical between `Meter` and `Sent`, every frame |

Per-piece numbers over 30 frames each, from the test's own output (device-side codec
and size, from `SimDevice::start_with`'s frame sink - the *receiver's* view, not ours):

| piece | displayed | exact / fallback | mean bytes | codec |
|---|---|---|---|---|
| `clocks-numerals` | 30/30 | 30 / 0 | 471 | `pal8-lz` |
| `plasma` | 30/30 | 30 / 0 | 1365 | `pal8-lz` |
| `metaballs` | 30/30 | 0 exact | 1164 | `pal8-lz` |

The tests send with `Cadence::Free` deliberately. The meter and the sender each hold
their own `Encoder`, and the chooser's hysteresis means two encoders agree only if they
are shown the same frames; one in, one out is what makes "the preview is what the panel
shows" checkable rather than usually-true. Under the default `Cadence::Limit` a 60 fps
piece coalesces half its frames and the meter's history would drift from the sender's -
worth knowing, harmless in practice (the drift can only change a *marginal* frame's
codec, never its exactness), and the reason the comparison is pinned this way.

`screeny-art play` against a headless `screeny-sim` on loopback, `--exit-after` on the
sim and `timeout` on the run, nothing left behind (`ps` clean):

```
screeny-art play plasma          --to 127.0.0.1:50600 --seconds 10 --seed 7
  600 offered, 300 sent, 300 coalesced, 0 dropped; exact 300 / fallback 0; pal8-lz ~1365 B
  device: rx 300 shown 300 gaps 0 stale 0 super 0 dec 0 rej 0, 30 fps
screeny-art play clocks-numerals --to 127.0.0.1:50604 --seconds 8  --seed 7
  480 offered, 240 sent, 240 coalesced, 0 dropped; exact 240 / fallback 0; pal8-lz ~600 B
  device: rx 240 shown 240, all counters 0, 30 fps
screeny-art play metaballs       --to 127.0.0.1:50602 --seconds 8  --seed 7
  456 offered, 228 sent, 228 coalesced, 0 dropped; exact 0; pal8-lz ~1100 B, bc1-dual on some frames
  device: rx 228 shown 228, all counters 0, 30 fps
screeny-art play clocks-numerals --to screeny-sim-101 --seconds 6  --seed 7   # by mDNS name
  360 offered, 180 sent, 180 coalesced, 0 dropped; exact 180 / fallback 0
  device: "lock released by 127.0.0.1:51695: Final", Live -> Hold
```

The thing worth reading twice: **`super 0`**. The piece renders at 60, the link puts 30
on the wire, and the device never superseded a frame. That is section 5's promise,
measured. `metaballs` offered 456 rather than 480 in eight seconds because
supersampling it costs more than a 60 Hz slot; the loop skips rather than bursting, as
it should.

**Finding: LAN unicast does not work from this worker's environment.** Sending to the
simulator at this Mac's own LAN address fails where loopback succeeds, and the
*reference* `screeny` binary fails identically - `screeny info --addr
192.168.7.203:50608` returns "no reply ... after 4 tries" and prints its own Local
Network hint, while `--addr 127.0.0.1:50608` answers instantly. So it is not this
card's code: it is macOS Local Network permission for processes this session starts.
mDNS browsing works (the name resolved to the right host and port); only the unicast
that follows is dropped. Consequence for the real-panel step: see the note at the end
of this log.
