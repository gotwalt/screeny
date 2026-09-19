# Architecture

Status: **accepted** (card 004, 2026-09-19), layout revised by card 016.
Wire format: `protocol-v1.md`.

## Repository layout

```
Cargo.toml            host workspace: members = crates/*  (stable toolchain)
crates/
  proto/              no_std, no alloc, no float. Shared by firmware and host.
                      Packet header, FRAME/CONTROL parse+build, control ops, telemetry
                      struct, DNS-SD TXT encode/parse, seq arithmetic, the frame types,
                      and the five v1 DECODERS (lifted from lab/src/dec). Test vectors
                      live here. Depends on nothing.
  receiver/           no_std, no alloc, no float. The RECEIVE STATE MACHINE: sections
                      3.3, 4.7, 6 and 7, the counters and EWMAs of 6.8, the source lock,
                      the rate limiters. No I/O and no clock; one `Host` trait carries
                      everything a receiver cannot decide for itself. Used by BOTH
                      firmware/ and sim/, which is the point (card 016).
  panel/              std. What the panel does to a colour: sRGB EOTF and inverse,
                      Oklab/OKLCH, and the panel's sRGB8 -> emitted-light transfer
                      function plus the brightness LUT. Lifted from lab/src/color.rs
                      and panel.rs. Used by encode/, demos/ and sim/.
  encode/             std. The ENCODERS and the per-frame codec chooser (lifted from
                      lab/src/enc). Frame + byte budget in, codec id + payload out,
                      guaranteed to fit and to decode. Split out of screeny/ by card 016
                      so demos/ can measure what a piece costs on the wire.
  screeny/            std. Library + the one `screeny` binary.
                      lib: discovery (mdns-sd), paced Sender, stats/telemetry client,
                      control client, host frame container; re-exports encode/ as
                      `screeny::encode` and panel/ as `screeny::panel`/`::color`.
                      bin subcommands: discover, info, stats, brightness, identify,
                      pattern, pipe (raw RGB888 on stdin), fractal, clock, (later) ddp-proxy.
  demos/              std. Pure renderers: fn(t, params) -> Frame. Fractal zoom, word
                      clock. No networking. Preview to PNG/GIF. Used by screeny's
                      `fractal` and `clock` subcommands.
  sim/                std. A fake panel: speaks the full protocol (frames, control,
                      telemetry, mDNS as `screeny-sim`), runs the shared receiver core,
                      decodes with `proto`, shows an LED-dot window, and has a headless
                      mode that dumps frames + stats for tests. The reference for anyone
                      without hardware access.
  probe/              std. `screeny-probe` (card 008), the bench instrument: streams
                      the checked-in vectors at a paced rate, drives the source lock
                      from two sockets, and probes the MUSTs only a *device* can fail.
                      Not the product - it sends payloads somebody else encoded and
                      reports what the device says it received. Prove changes against
                      `sim` before pointing it at hardware.
firmware/             its own cargo project (esp toolchain, xtensa-esp32-none-elf),
                      NOT a workspace member. Depends on crates/proto and
                      crates/receiver by path.
lab/                  card 002's codec lab. Frozen reference; not a workspace member.
tools/                bench scripts (flash+log+snap, camera daemon, backup).
```

The dependency graph is a line, and the direction matters:

```
proto ──┬── receiver ──────────────┬── sim
        ├── panel ──┬── encode ──┬─┴── demos ── screeny (lib + bin)
        │           │            └───────────────┘
        └────────────────────────────────────────── probe
```

**`screeny` depends on `demos`** (the `fractal` and `clock` subcommands), so demos
can never depend on `screeny`. Anything the two share therefore goes *below* both -
which is why `panel/` and `encode/` are their own crates rather than modules of
`screeny`. Their old paths still exist as re-exports, so `screeny::encode`,
`screeny::panel` and `screeny::color` mean what they always did.

One host binary on purpose: macOS Local Network permission and code signing are per
binary identity (card 015), so everything that talks to the LAN lives in `screeny`.
(`preview` and `screeny-sim` are binaries too; neither one sends frames to a device.)

## One implementation of each shared thing

Card 016's rule, and the reason for the crate count: the receive state machine, the
panel transfer function, the sRGB/Oklab conversions, the frame types and the codec
names each exist **once**. The build phase produced two copies of the first
(`crates/sim` and `firmware/`, ported by hand with a comment asking the next person
to diff them) and three of the next two. A second copy of a spec behaviour is a
second reading of the spec, and the day they disagree is the day a device and its
simulator stop being evidence about each other.

## Frame types (in `proto`, no_std)

```rust
pub const W: usize = 64; pub const H: usize = 32; pub const NPIX: usize = 2048;
pub type Rgb888Frame = [u8; NPIX * 3];            // sRGB, row-major, top-left origin
pub struct IndexedFrame<'a> { pub palette: &'a [[u8; 3]], pub indices: &'a [u8; NPIX] }
```

Everything upstream of the panel driver is **sRGB 8-bit**. Linear light exists in
exactly two places: inside renderers that blend (their own business), and in the
firmware's final sRGB8 -> panel duty lookup.

These are the *only* frame types. `screeny::Frame` and `demos::Frame` are each a
`Box<Rgb888Frame>` with convenience methods and a `Deref` to it, so a `&Frame`
works wherever a `&Rgb888Frame` is wanted; `demos::Indexed` is the owned form of
`IndexedFrame` and hands out the borrowed one with `as_proto()`.

## The receiver core (in `receiver`, no_std)

`firmware/` and `crates/sim` run the same state machine (card 016). It owns the
source lock, the sequence test, the counters, the two EWMAs, the three rate
limiters, the `IDENTIFY` overlay and the idle timers, and it does no I/O:

```rust
let offer = rx.offer_frame(&mut host, now_us, from, datagram); // Keep | Drop
// ... drain until the socket would block, keeping the last Keep ...
let published = rx.flush_frames(&mut host, now_us, survivor, &mut back);
```

The caller keeps the survivor datagram and hands it back at flush, so the core
allocates nothing and the device copies nothing. Everything environmental - the
clock, where a datagram goes, what happens to a decoded frame, whether there is a
radio, what a log line looks like - is a method on the `Host` trait, and
`Receiver<A>` is generic over the address type alone (`SocketAddr` on the host,
`IpEndpoint` on the device). The fixed-size shape is the firmware's: four-slot
rate limiters with oldest-entry eviction, two pending `STATS_REQ` slots, stack
buffers for the two unsolicited packets of section 6.2.

## Firmware

Embassy tasks on esp-rtos, **split across both cores** (card 008):

| Core | Task | Job |
|---|---|---|
| 1 | `display` | owns esp-hub75 + both DMA framebuffers; converts the front sRGB888 frame through the gamma LUT and the dither phase into the inactive DMA buffer and swaps at a refresh boundary. Takes new frames from the triple buffer; blocks on nothing |
| 0 | `wifi` | associate, reconnect forever, modem sleep off, poll RSSI |
| 0 | `net` | embassy-net runner |
| 0 | `frames` | UDP 49374: drain the socket until it would block, keep the newest valid FRAME for the active source, decode into the display's back buffer (never partially visible), publish. Owns the source lock, the idle state machine, and therefore also the idle screen, the cross-fade and the IDENTIFY overlay |
| 0 | `control` | UDP 49375: every opcode in protocol-v1 section 6.3 |
| 0 | `mdns` | `_screeny._udp` responder; TXT built by parsing the GET_INFO body back out, so the two cannot drift. Re-announces on SET_NAME |
| 0 | `telemetry` | the serial-log line a camera cannot measure. The protocol's own telemetry is section 6.7 and leaves by the sockets above |

There is no `status` task: a state machine that knows whether a source holds the
lock is also what knows whether the idle screen is wanted, so the `frames` task
composes it and is the **single writer** to the display.

**The handoff between the two halves is `firmware/src/fb.rs`: a lock-free triple
buffer.** Three 6 KB sRGB slots and one atomic word; publish and acquire are one
swap each. No mutex, no copy, and no critical section to disturb the `Priority3`
refresh ISR. The invariant that makes it sound is that the producer's slot index
and the consumer's are never equal, so `back()` and `front()` cannot alias — which
is also why tearing is structurally impossible rather than merely unobserved.

Card 008 measured the split itself as *neutral* at 30 fps and up to 120 fps: the
lock-free handoff is what fixed card 007's "newest wins can never fire", and the
core split on top of it buys headroom (core 0's display cost drops from ~48% to
~2%) that nothing needs yet. `--features display-on-core0` keeps the A/B build.

Memory on this chip is tighter than card 001 assumed, and the reason is worth
carrying: **`.bss` and core 0's main stack come out of the same pocket**, the
region between `_bss_end` and `0x3ffe0000`, while the 64 KB reclaimed-ROM heap
sits above the stack where it cannot help. Budget:

| | |
|---|---|
| two DMA framebuffers | 2 x 12,312 B (`.bss`) |
| triple buffer + cross-fade source | 4 x 6,144 B (`.bss`) |
| core 1's stack | 16 KB (`.bss`) |
| heap | 64 KB reclaimed + 32 KB `.bss`; 45.4 KB in use |
| core 0's main stack | 37,744 B, and it must hold a 12 KB `FrameBuffer::new()` temporary |

Decoders need no scratch beyond `decode_pal8_lz`'s 768-byte stack palette. Card
016's move to the shared receiver core cost 192 bytes of core 0's main stack and
no `.bss` at all; heap in use is unchanged at 45,344 B.

Display quality roadmap, in order: gamma LUT (required for v1) -> ghosting fix ->
brightness without losing bit depth (card 020) -> temporal dithering across refreshes
(card 030). All four are done. Codec selection in the sender assumes the dithered panel.

Bench levers (gamma, dither, output-enable window, held test patterns) are private
control opcode `0x80`, which section 6.3 reserves for exactly that. They are
deliberately **not** on the frame port: every datagram arriving on 49374 has to be
either a conforming frame or a `frames_rejected`, or the counter a sender reads to
diagnose its video stream stops meaning anything.

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
- `receiver`: the simulator's suite *is* this crate's suite. `crates/sim`'s tests
  drive the shared core through a std `Host`, so a change to the state machine that
  the device would fail fails on a laptop first, in microseconds of virtual time.
- `screeny` <-> `sim` over loopback in CI-style tests: discovery, streaming, control,
  loss injection.
- Hardware: `tools/fw-run.sh` for flash+log+snap; card 012 builds the camera
  comparison harness; card 013 measures fps/latency/loss end to end.
