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
