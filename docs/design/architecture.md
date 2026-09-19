# Architecture

Status: **accepted** (card 004, 2026-09-19). Wire format: `protocol-v1.md`.

## Repository layout

```
Cargo.toml            host workspace: members = crates/*  (stable toolchain)
crates/
  proto/              no_std, no alloc, no float. Shared by firmware and host.
                      Packet header, FRAME/CONTROL parse+build, control ops, telemetry
                      struct, DNS-SD TXT encode/parse, seq arithmetic, and the five v1
                      DECODERS (lifted from lab/src/dec). Test vectors live here.
  screeny/            std. Library + the one `screeny` binary.
                      lib: ENCODERS + per-frame codec chooser + panel model (lifted from
                      lab/src/enc, panel.rs, color.rs), discovery (mdns-sd), paced
                      Sender, stats/telemetry client, control client.
                      bin subcommands: discover, info, stats, brightness, identify,
                      pattern, pipe (raw RGB888 on stdin), fractal, clock, (later) ddp-proxy.
  demos/              std. Pure renderers: fn(t, params) -> Frame. Fractal zoom, word
                      clock. No networking. Preview to PNG/GIF. Used by screeny's
                      `fractal` and `clock` subcommands.
  sim/                std. A fake panel: speaks the full protocol (frames, control,
                      telemetry, mDNS as `screeny-sim`), decodes with `proto`, shows an
                      LED-dot window, and has a headless mode that dumps frames + stats
                      for tests. The reference for anyone without hardware access.
firmware/             its own cargo project (esp toolchain, xtensa-esp32-none-elf),
                      NOT a workspace member. Depends on crates/proto by path.
lab/                  card 002's codec lab. Frozen reference; not a workspace member.
spike/fw-skeleton/    card 001's bring-up spike. Frozen once firmware/ exists.
tools/                bench scripts (flash+log+snap, camera daemon, backup).
```

One host binary on purpose: macOS Local Network permission and code signing are per
binary identity (card 015), so everything that talks to the LAN lives in `screeny`.

## Frame types (in `proto`, no_std)

```rust
pub const W: usize = 64; pub const H: usize = 32; pub const NPIX: usize = 2048;
pub type Rgb888Frame = [u8; NPIX * 3];            // sRGB, row-major, top-left origin
pub struct IndexedFrame<'a> { pub palette: &'a [[u8; 3]], pub indices: &'a [u8; NPIX] }
```

Everything upstream of the panel driver is **sRGB 8-bit**. Linear light exists in
exactly two places: inside renderers that blend (their own business), and in the
firmware's final sRGB8 -> panel duty lookup.

## Firmware

Embassy tasks on esp-rtos, all on core 0 unless measurement says otherwise:

| Task | Job |
|---|---|
| `display` | owns esp-hub75 + two DMA framebuffers; on a "new frame" signal converts the decoded RGB888 back buffer through the gamma/brightness LUT into the inactive DMA buffer and swaps at a refresh boundary |
| `wifi` | associate, reconnect forever, modem sleep off |
| `net` | embassy-net runner |
| `frames` | UDP 49374: drain socket, keep newest valid FRAME for the active source, decode into the RGB888 back buffer (never partially visible), signal `display`; source lock + idle timeout state machine |
| `control` | UDP 49375: GET_INFO, PING, telemetry, brightness, identify, reboot, SET_WIFI |
| `mdns` | `_screeny._udp` responder, TXT from the same bytes as GET_INFO |
| `status` | draws boot / connecting / IP / idle screens with a bitmap font when no stream is active |

Memory (card 001): two 12.3 KB DMA framebuffers + one 6 KB RGB888 decode buffer +
WiFi heap. Decoders need no scratch.

Display quality roadmap, in order: gamma LUT (required for v1) -> ghosting fix ->
brightness without losing bit depth (card 020) -> temporal dithering across refreshes
(card 030). Codec selection in the sender already assumes the dithered panel.

## Sender

```
source (pipe | demo | pattern) -> Frame -> chooser (encode 3 ways, score, hysteresis)
   -> pacer (monotonic 33.33 ms ticks, skip never burst) -> UDP unicast
   <- piggybacked telemetry (STATS_REQ every N frames) -> adaptive fps ladder / stats
```

An `IndexedFrame` with <= 32 colours bypasses the chooser: `PAL4_LZ`/`PAL5` is exact.

## Testing strategy

- `proto`: round-trip and golden vectors for every packet type and codec; fuzz-style
  tests that no input can panic or index out of bounds (it parses network input on a
  device with no MMU).
- `screeny` <-> `sim` over loopback in CI-style tests: discovery, streaming, control,
  loss injection.
- Hardware: `tools/fw-run.sh` for flash+log+snap; card 012 builds the camera
  comparison harness; card 013 measures fps/latency/loss end to end.
