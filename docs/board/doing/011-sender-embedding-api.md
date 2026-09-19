---
id: 011
title: Make crates/screeny a good library to embed (the art system becomes the primary sender)
type: build
hardware: no
depends: [009]
owner: card-011 worker
branch: card/011-sender-embedding-api
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

### Indexed frames through the `Sender` (gap 1)

`Pixels<'a>` (in `frame.rs`) is the frame type the push API takes: `Rgb(&[u8])`
or `Indexed { palette: &[[u8;3]], indices: &[u8] }`. **Slices, not arrays**,
because the art system keeps `WireFrame.rgb` and its indices in `Vec`s and
`&Vec<u8> -> &[u8; 6144]` at every call site is exactly the sharp edge this
card exists to file off. `From<&Frame>` and `From<&Rgb888Frame>` are there too.

- `Sender::send(Pixels) -> Result<Sent>` is the one door; `Sender::send_indexed
  (&[[u8;3]], &[u8])` is the direct form. `send_frame` is untouched, so `run`
  and the CLI are unaffected.
- `Sent` is an enum, not a codec byte: `Frame { codec, bytes, exact, seq }`,
  `Coalesced`, `Dropped`. A producer can see that a frame did not reach the
  panel without the library having to log anything.
- Exactness is `Encoder::encode_indexed`'s existing ladder; what was missing
  was a way to reach it from the sender. <= 16 colours -> `PAL4_LZ`, else
  `PAL8_LZ`, else (<= 32 colours) raw `PAL5`, which is fixed-rate at 1376
  bytes and so cannot overflow. That is why "up to 32 colours is exact" holds
  for *any* index plane, including noise.
- The over-budget fallback (> 32 colours and incompressible, or a budget under
  1376) expands to RGB and runs the lossy chooser - a requantised frame beats a
  dropped one - and says so three ways: `Sent::exact` false,
  `SendStats::indexed_fallback`, `SendStats::last_fallback_colours`.
- `Error::Frame` and `Error::BadIndex` name the caller's own mistakes, and
  `impl From<Error> for std::io::Error` exists so the art system's
  `Output::send(&mut self, &WireFrame) -> io::Result<()>` is a `?` away. An
  `Error::Io` keeps its kind and errno through that conversion.

`crates/screeny/tests/indexed.rs`: 16 tests, 0.16 s, all against
`screeny_sim::SimDevice` on loopback with ephemeral ports and mDNS off
(`tests/simfix/mod.rs` is the fixture). Palettes of 2, 16, 17, 32 colours with
structured indices *and* with LCG index noise; a 30-frame stream; 200 colours
compressible (exact, `PAL8_LZ`) and incompressible (falls back); a budget below
`MIN_BUDGET`. The assertion is always `decoded == palette[index]` for all 2048
pixels, checked by the *second* implementation of the protocol.

Surprising, and worth knowing: with 17-32 colours and incompressible indices,
`PAL8_LZ` is tried before `PAL5` and *expands* to ~2100 bytes, so the frame
lands on `PAL5` at 1376. Correct, and it means the exact path's worst case is
1376 bytes, not the LZ rungs' average ~300.

Not my change, but found while running the suite: `SCREENY_PACING_SECS=2`
fails `holds_thirty_fps_within_one_percent`, because one frame is 1.6% of a
two-second window and the tolerance is 1%. The default ten-second run passes
(18.4 s wall). Written up as card 093.

### The push API and auto-reconnect (gaps 2, 3, 5)

`crates/screeny/src/embed.rs`. `Sender` stays exactly what it was - one
socket, one session, every failure handed back - and `Link` is the wrapper a
long-running embedder gets:

```rust
Link::open(Target, LinkConfig) -> Result<Link>     // blocking; fails if absent
Link::open_deferred(Target, LinkConfig) -> Link    // never fails
Link::send(Pixels) -> Result<Sent>                 // network cannot make this fail
Link::poll() / close() / retarget(Target)
Link::state() -> LinkState { Up, Connecting, Waiting, Closed }
Link::stats() -> &LinkStats      // lifetime, across sessions
Link::session() -> Option<&SendStats>   // this socket's detail
Link::limits() -> Limits         // fps, budget, exact_palette, codecs, panel
Link::fps() / pacer() -> Pace
```

Decisions, and why:

- **Pacing is the caller's.** `Link` never sleeps. The art system has its own
  clock, limiter and reasons to render; a library sleeping on its behalf would
  fight it. `Pace` (spec 9.1's absolute schedule, skip-never-burst) is offered
  and entirely opt-in - `link.pacer()` hands one back at the current rate.
- **Pushing too fast is answered by decimating, not by sending.** `Cadence::
  Limit` (the default) keeps an absolute schedule at the rate the sender is
  targeting and returns `Sent::Coalesced` for frames that arrive early. The
  device shows newest-wins and queues ~5 frames, so a surplus buys nothing and
  costs latency - and spec 6.9 reads `frames_dropped_superseded` as "cannot
  decode fast enough" and switches to *cheaper codecs*, which is the wrong
  repair for "the sender is too eager". Measured: a 60 fps producer sends
  ~30 fps and the simulator's superseded counter stays at **0**. `Cadence::Free`
  hands it all back.
- **The rate is the device's, not a constant.** The ceiling follows
  `Sender::fps()`, which is `SenderConfig::fps` (default 30) stepped by spec
  6.9's ladder. `Limits` makes that, the budget, the codec set and the
  guaranteed-exact palette size visible, and every field can change under a
  running link.
- **The watchdog is the interesting part of reconnection.** UDP to a dead host
  succeeds, so a rebooted panel looks exactly like a working one. `Sender`
  now records `last_feedback()` - *any* datagram from the device, telemetry or
  `BUSY` - and `Link` declares the link down after `LinkConfig::silence`
  (default 5 s) of nothing heard *while actively sending*. Socket errors
  (ICMP coming home) do it immediately.
- **Reconnecting happens on a background thread**, because resolve + handshake
  blocks for up to 3 s + 750 ms and a render loop must not. Measured: 200
  `send`s into a black hole (TEST-NET-3) take under 500 ms. Backoff is
  immediate, then 250 ms doubling to 10 s.
- **`FINAL` on drop**, via `Drop for Link`. Test: the simulator's
  `active_source` clears at once instead of waiting `STREAM_TIMEOUT_MS`.
- `Link::retarget` moves a live link to a different device, keeping lifetime
  statistics. A `Target` naming an *instance* needs no retargeting: each
  reconnect re-browses and picks up the new address. That mDNS path is not
  covered by a test, because these tests run with mDNS off by rule.

`crates/screeny/tests/embed.rs`: 13 tests, 1.0 s. Reboot on the same ports
(sim dropped and restarted mid-stream), migration to a *different* port pair
through `retarget`, silence alone bringing the link down, reconnection turned
off, a deferred link connecting when the panel appears, `FINAL` on drop, the
60 fps decimation, and the malformed-frame errors still being the caller's.
Throughout, the invariant asserted is
`frames_offered == frames_sent + frames_coalesced + frames_dropped`: not one
frame ever became an error the embedder had to handle.

Two test-fixture notes worth keeping: `sim_anywhere()` retries the
port-pair choice, because tests run in parallel and a pair chosen and then
bound has a race; and the sim must be started with `control = frame + 1` for a
bare `Target { addr }` to find both halves, which is what an embedder on the
bench actually has.

### The examples

`crates/screeny/examples/embed.rs` - the ten-line embedding is one function:

```rust
fn stream(target: Target, art: &mut impl FnMut(f64) -> (Vec<[u8; 3]>, Vec<u8>)) -> screeny::Result<()> {
    let mut link = Link::open(target, LinkConfig::default())?;
    let mut pace = link.pacer();
    loop {
        let t = pace.tick();
        let (palette, indices) = art(t.secs());
        link.send(Pixels::indexed(&palette, &indices))?;
    }
}
```

The rest of the file is argument parsing and a stripe pattern to look at.
Run against a simulator on loopback (`--frame-port 49400`, mDNS off):
**29.6-30.5 fps, `PAL4_LZ`, 213 B/frame, gaps 0, superseded 0, decode
failures 0, rejected 0.**

`crates/screeny/examples/art_output.rs` - the art system's `Output` impl,
with their `Output` trait and `WireFrame` restated locally as stand-ins so it
compiles here and `art/` is untouched. The impl is fifteen lines:

```rust
impl Output for SenderOutput {
    fn send(&mut self, frame: &WireFrame) -> io::Result<()> {
        let px = match &frame.indexed {
            Some((palette, indices)) => Pixels::indexed(palette, indices),
            None => Pixels::rgb(&frame.rgb),
        };
        self.link.send(px)?;
        Ok(())
    }
}
```

Run at their 60 fps against the simulator: **360 offered / 180 sent / 179
coalesced / 1 dropped, 180 exact, `PAL8_LZ` 575 B, superseded 0.** The one
dropped frame is the first, pushed while the deferred link was still
connecting. That is the whole design working: their loop stays at 60, the
panel gets a clean 30, nothing is superseded, and every frame is exact.

Worth knowing, and now in the README: `Drop` sends `FINAL` on a normal return
or an unwind, but **not** on `SIGTERM` or ctrl-c, which terminate without
running destructors. An embedder that wants the lock released promptly on a
signal needs its own handler, the way the CLI does. In the runs above the
simulator logged `lock released by ...: Timeout` for exactly that reason,
because `timeout(1)` killed the example.

### Linux (gap 4)

`rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu` (both
were in fact already installed), then:

| | |
|---|---|
| `cargo check -p screeny --target x86_64-unknown-linux-gnu` | clean, no warnings |
| `cargo check -p screeny --target aarch64-unknown-linux-gnu` | clean, no warnings |
| `cargo clippy -p screeny --all-targets --target <both>` | clean |

`--all-targets` includes the new sim-backed tests and both examples, which
matters because it pulls `screeny-sim` in as well; it is a dev-dependency with
`default-features = false`, so `minifb` (and X11) never enter the Linux build.

**Nothing needed fixing**, which deserves a check of its own rather than a
shrug, because "it compiled" and "the `cfg(target_os = "linux")` branch was
compiled" are different claims. Proved the second directly: a temporary
`compile_error!` inside the `#[cfg(target_os = "linux")]` arm of
`net::set_dontfrag` fires on both Linux targets and is silent on macOS. So
`IP_MTU_DISCOVER`/`IP_PMTUDISC_DO` really is the code being built there, and
the macOS `IP_DONTFRAG = 28` really is not. Probe reverted.

The other `unsafe` path, `net::local_ipv4`'s `getifaddrs`, is portable as
written: `sa_family` is `u8` on macOS and `u16` on Linux and the code casts to
`c_int`, and it already null-checks `ifa_netmask`, which Linux leaves null for
some interfaces.

**mDNS on headless Linux needs no avahi.** `cargo tree -p mdns-sd --target
x86_64-unknown-linux-gnu` is `fastrand`, `flume`, `if-addrs`, `log`, `mio`,
`socket-pktinfo`, `socket2` - every one pure Rust over `libc`, with no `-sys`
crate, no build script linking anything, and no dbus, X11 or Wayland anywhere
in `screeny`'s Linux tree. It is its own responder, not a client of one. Two
things a deployment still has to get right, neither testable from this bench:
a box already running `avahi-daemon` has something else bound to UDP 5353, and
a container on Docker's default bridge network sees no multicast at all. Both
fail the same survivable way an empty browse always does - and `--addr` and
`--broadcast` both work regardless, which is why discovery is never the only
route to a device.

### Documentation

`crates/screeny/README.md` gains an **Embedding** section immediately after the
intro: the toml line, the ten-line loop, "the five things worth knowing"
(exactness and the fallback, the network cannot fail `send`, reconnection is
automatic and off-thread, pacing is yours and overrunning is handled, the frame
rate is a choice that moves), the API at a glance, and the two sharp edges that
are genuinely left (`Drop` does not run on a signal; one panel, one sender).
The "library in one screen" block and the test table are updated to match, and
the testing preamble now says that `tests/simfix` drives a real `screeny-sim` -
which is the point, because an exactness claim checked by the encoders' own
crate is worth nothing.

`#![warn(missing_docs)]` and `#![warn(clippy::pedantic)]` were already on the
library and still pass; `cargo doc -p screeny --no-deps` produces no warning
from any new code. It does produce four pre-existing ones in
`encode/quant.rs` (public docs linking to private `SEED_DRIFT` /
`RESEED_EVERY`); left alone as out of scope, written up as card 092.

`docs/design/generative-art-brief.md` section 5 is rewritten. It was
"[provisional], two hand-over formats are planned, build a trait so either can
be plugged in". It is now "[decided], the sender exists, link it", the five-line
`Output` body, and seven points chosen for what should actually change in their
code: exactness is measured rather than hoped for; 256 colours are exact when
they compress, so the 32 in section 2.3 is the guaranteed size and not the
limit; the fallback is visible and belongs on the studio's stats strip;
`Sent::bytes()`/`codec()` replace `budget.rs`'s estimates with real numbers;
**keep the 60 fps loop** and let the link decimate rather than dropping to 30;
the panel going away is not their problem; and let the link drop to send
`FINAL`. The single-owner hardware rule is restated, because linking the sender
is exactly the moment someone might think it no longer applies. The "Build a
faithful preview first" subsection is kept as it was.
