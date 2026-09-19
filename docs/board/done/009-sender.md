---
id: 009
title: crates/screeny - sender library and CLI
type: build
hardware: no
depends: [005]
owner: card-009 worker
branch: card/009-sender
---

## Goal

The host side: a library that takes frames and gets the best possible picture onto
the panel at a steady 30 fps, and the one `screeny` binary that wraps it.

## Context

- Spec: `docs/design/protocol-v1.md` (sections 4.8, 5, 6, 9 especially).
  Architecture: `docs/design/architecture.md`. Wire types/decoders: `crates/proto`.
- Encoders, the per-frame chooser, the panel model and Oklab scoring exist and are
  measured in `lab/src/enc/`, `lab/src/panel.rs`, `lab/src/color.rs`,
  `lab/src/metrics.rs` (report: `docs/research/002-frame-encoding.md`). Lift them;
  `lab/` itself stays frozen. **Wire difference:** the lab prepends a mode byte to each
  payload; on the wire the codec id is in the header only. Strip it at the boundary,
  and verify every encoder's output decodes through `screeny_proto::dec::decode`
  to exactly what the lab's own decoder produced.
- Card 031 (encode-time budget) is yours too: the lab's chooser took ~8 ms/frame
  unprofiled. Measure it in release mode; it must leave comfortable headroom inside
  33 ms alongside rendering. Cheap wins: skip the chooser for frames with <= 16 / <= 32
  distinct colours (exact by construction), reuse the previous palette as a seed, bail
  out of the ladder early. Report before/after.
- The device is reachable by mDNS as `screeny` (`_screeny._udp`, frames 49374, control
  49375) - but the real firmware does not speak the final protocol yet (card 008), and
  a simulator is being built in parallel (card 006). So test against an in-process
  receiver written directly on `crates/proto` (a few lines: parse, decode, compare).
  Do not send anything to the real device from this card.
- A demos crate (`crates/demos`: fractal zoom, word clock) is being built in parallel
  on another branch. Leave a clean seam - `trait FrameSource` - and the orchestrator
  will wire `screeny fractal` / `screeny clock` after both merge.
- macOS: senders should set `IP_DONTFRAG`; pace with a monotonic clock; skip, never
  burst. Local Network permission/signing is card 015, not this card, but make the
  failure modes visible: on `EHOSTUNREACH`/`ENETUNREACH` to a same-subnet address or an
  empty browse, print a hint that names System Settings > Privacy & Security > Local
  Network, and always allow `--addr IP[:port]` to bypass discovery.

## Deliverables

`crates/screeny` (package `screeny`, lib + bin `screeny`):

- `encode`: the four encoders + SOLID, the palette ladder, the chooser with hysteresis,
  panel model scoring. API takes `&Rgb888Frame` or an `IndexedFrame` and a byte budget,
  returns `(codec_id, payload)`.
- `discover`: browse `_screeny._udp` with `mdns-sd`, parse TXT via proto's `DeviceInfo`,
  pick by instance name; timeout + helpful error.
- `Sender`: connects to a device (discovered or `--addr`), `GET_INFO` handshake to learn
  codecs/mtu, paced send loop at a target fps from a `FrameSource`, seq numbers,
  `STATS_REQ` every N frames, telemetry-driven adaptation per the spec (network-limited
  -> step fps down; decode-limited -> cheaper codec), `FINAL` on clean shutdown
  (ctrl-c handled), live stats.
- CLI subcommands: `discover`, `info`, `stats` (live telemetry), `brightness N`,
  `identify`, `reboot`, `ping` (RTT stats), `pattern <name>` (bars, grey ramp, gradient,
  orientation "F", checker, moving sweep - these double as the bench test patterns),
  `pipe` (raw RGB888 6144-byte frames on stdin, `--fps`), `encode-stats` (run the
  chooser over stdin frames and print codec/bytes/ms without sending).
- Tests: every encoder round-trips through proto's decoder; nothing ever exceeds the
  budget (property test over random and adversarial frames, several budgets down to
  1376); chooser picks exact modes for low-colour frames; pacer holds 30.0 fps +-1%
  over 10 s and never bursts after a stall; loopback end-to-end against the in-process
  receiver including loss.
- README for the crate: library quick start + every CLI command.

## Acceptance

`cargo test -p screeny` passes; `screeny pattern bars --addr 127.0.0.1:PORT` drives an
in-process or simulator receiver at a steady 30 fps; measured encode time per frame is
reported in the card log with headroom inside 33 ms.

## Log

### 2026-09-19 - built, measured, in review

`crates/screeny` is there: library plus the one `screeny` binary, 52 tests
plus the doc examples, clippy clean under `pedantic`. Card 031 was folded in
and its numbers are in its own log.

**Nothing in this card was pointed at the bench device**; the one
exception is `screeny discover`, which browses mDNS read-only, and what it
found is at the end of this entry.

**What was lifted, and what changed on the way.** The encoders, the ladder,
the chooser, the panel model and the Oklab scoring all come from
`lab/src/enc`, `panel.rs`, `color.rs` and `metrics.rs`. The wire difference is
handled at the boundary: `Encoded` carries `{ codec, payload }` separately and
no payload contains a mode byte. The lab's uncompressed 4 bpp mode is not a v1
codec so it did not come across, and neither did Floyd-Steinberg, which is not
on the chooser's menu. Two things were *added* to the ladder:

- a raw `PAL5` rung on the frame's own colours when there are 32 or fewer.
  Both LZ rungs can overflow on an incompressible index plane - random
  16-colour noise is 1200 bytes after LZ, random 32-colour noise does not fit
  at all - and `PAL5` is fixed-rate, so a frame of 32 colours or fewer is now
  exact *whatever* its content. The lab would have dropped to a dithered
  palette there and lost exactness for no reason.
- `SOLID`, which the lab's hybrid never emitted.

**Cross-checked against the frozen lab.** A harness outside the repo depends
on both `lab` and `crates/screeny`, generates the lab's five clips, and runs
both encoders over identical frames. Every payload `crates/screeny` produces
decodes through `screeny_proto::decode`, and separately every payload the
*lab* produces decodes through `screeny_proto` to exactly what the lab's own
decoder produces once the mode byte is dropped - which is the check this card
asked for, and it passes on all 300 frames. Codec choice agrees with the lab
on 85-100% of frames per clip, and mean panel-aware dE comes out slightly
ahead of the lab's (6.08 against 6.14). Payloads are byte-identical on the two
clips where the palette is deterministic (`textui`, `photo`) and differ
elsewhere, because the quantiser is seeded differently on purpose; card 031's
log has the reasoning.

**Pacing.** Measured against the in-process receiver over ten seconds:
**30.00 fps, within 1% at both ends**, no drift beyond two periods across 300
frames, and no two datagrams closer than a third of a frame period. A 300 ms
stall mid-stream is skipped, not caught up: nine frames are dropped from the
schedule, the stream resumes on the next whole slot, and sequence numbers stay
contiguous because they count frames *sent*. 10, 24 and 60 fps behave the
same.

One spec problem found here. Section 9.1's pseudo-code sets `n = should_be`
after a stall, which leaves the next target at approximately *now* - so the
recovery frame goes out 55 microseconds behind the stalled one. That is a
two-frame burst, which is the one thing the surrounding paragraph forbids. The
sender resumes at the next whole slot instead (`should_be + 1`), and card 090
folds the fix back into the spec.

**Telemetry and adaptation.** `STATS_REQ` rides on about one frame a second,
never two within 100 ms, so the steady state is one packet per frame. All four
of section 6.9's rules are implemented and the first two are tested: 25% loss
injected at the receiver steps the rate down the 30/24/20/15 ladder within
three seconds, a clean link leaves it alone for the whole run, superseded
frames drop the codec set to the fixed-rate ones rather than lowering the
frame rate, and a decode failure withdraws that codec and says out loud that
one of the two implementations is wrong.

**Local Network failure modes**, as the card asked, without straying into card
015: every error that could be caused by the macOS permission carries a hint
naming System Settings > Privacy & Security > Local Network, explaining that a
Terminal-started CLI is exempt but a re-signed or launchd-started binary is
not, and that closing the Terminal window can revoke the exemption mid-run. An
empty browse is never fatal - it prints the hint plus `dns-sd -B _screeny._udp`
as the way to tell "not advertising" from "cannot see multicast" - and
`--addr` bypasses discovery on every subcommand. `ECONNREFUSED` gets a
different hint, because "nothing is listening on that port" is a different
problem from "the OS will not route to your LAN".

**Testing.** `tests/common` is a fake device built on `crates/proto`: two
loopback sockets on ephemeral ports, the section 2 and 3 validation rules,
proto's decoders, `TELEMETRY` sent back out of the *frame* socket per section
6.4, the control opcodes, and a loss injector. 52 tests over five files and
the crate itself, plus the doc examples. `cargo test -p screeny` passes in
debug and release alike; the pacing file takes 18 s of the run and
`SCREENY_PACING_SECS` shortens it.

`cargo test` in debug was a real constraint: a debug encoder cannot hold 30
fps, so the pacing tests use a source whose frames take the `SOLID` shortcut.
The README says to use `--release` for anything that streams.

**The seam for card 010.** `trait FrameSource { fn render(&mut self, t:
FrameTime, out: &mut Frame) -> bool }` - render into a caller-owned buffer, so
a 30 fps stream does not allocate 6 KB a frame, and return `false` to end the
stream. `Pattern` implements it, `RawReader` implements it for `screeny pipe`,
and `FnSource` wraps a closure. Wiring `screeny fractal` / `screeny clock` is
adding two subcommands that construct a demo and hand it to `stream_source`.

**What `screeny discover` found on the bench, read-only.** The device is
advertising, and the browse and TXT parse work end to end:

```
screeny
  frames   192.168.7.221:49374
  control  192.168.7.221:49375
  host     screeny.local.
  panel    64x32
  codecs   "raw" - nothing this sender can produce, so it cannot stream
  mtu      1464
```

`dns-sd -L` confirms the raw record: `proto=1 w=64 h=32 ctrl=49375 codecs=raw`
- no `txtvers`, no `mtu`, no `fw`, no `id`, and a `codecs` value from before
the codec table existed. That is the card 001 bring-up firmware and card 008
is the card that fixes it; what matters here is that the sender handles it
correctly, which is pinned by a test: it discovers the device, prints exactly
what it saw, and `Sender::connect` refuses with `NoCommonCodec` rather than
guessing a codec and sending it. (An hour later the device had stopped
advertising altogether - `dns-sd -B` saw nothing either - which is presumably
another card's work on the bench.)

**Left undone**, deliberately:

- `screeny fractal` / `screeny clock` wait for card 010 to merge.
- Integration against the card 006 simulator waits for it to merge; the
  in-process receiver covers the same ground for now.
- Streaming to several panels at once is spec section 11's "sender-side only"
  decision with no card behind it. Now card 091.
- `--qos` sets `IP_TOS` to AF41 and is off by default. `SO_NET_SERVICE_TYPE`
  and whether the bench AP honours DSCP on the downlink is card 013.
- The fast profile's quality cost is measured against the card 002 synthetic
  clips only. Card 032 is the real-content corpus.
