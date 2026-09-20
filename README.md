# screeny

A Tidbyt (Gen 1: ESP32 + 64x32 RGB LED panel) turned into a network frame buffer.
Custom `no_std` Rust firmware on embassy receives one frame per UDP datagram over WiFi
and shows it; a Rust sender on the host discovers the panel by mDNS, compresses each
frame to fit a single packet, and streams at 30 fps.

```
 renderer ──> screeny (encode: pick best of 4 codecs, <= 1464 B) ──UDP/WiFi──> firmware ──> HUB75 panel
   crates/art, demos, pipe     crates/screeny                                   firmware/
                                        └────────── crates/proto (shared wire format + decoders) ─────┘
```

Measured on the bench: every codec holds 30 fps with zero decode drops and under 1%
network loss; decode takes 0.2-0.8 ms; ping RTT median 5 ms; the panel refreshes at
154 Hz with gamma correction, temporal dithering and depth-preserving brightness.
Details: `docs/research/005-end-to-end.md`.

## Layout

| Path | What |
|---|---|
| `crates/proto` | `no_std` wire format: packets, control ops, telemetry, the five frame decoders. Shared by firmware and host. |
| `crates/screeny` | Sender library + the `screeny` CLI (discover, stats, brightness, patterns, `pipe`, demos). |
| `crates/demos` | Reference renderers: endless fractal zoom, word clock. |
| `crates/sim` | A fake panel that speaks the whole protocol; LED-dot window or headless. |
| `crates/probe` | `screeny-probe`, bench instrument: the wire-level conformance suite, paced vector streams. A library too, so `crates/sim`'s tests drive the same sender, encoders and rules. |
| `firmware/` | The ESP32 firmware (separate cargo project, `esp` toolchain). |
| `crates/art` | The generative art system: pieces, panel-aware pipeline, headless `screeny-art` binary. The primary source of frames. |
| `crates/studio` | Screeny Studio: an HTTP + WebSocket server with the UI built in. `/` designs pieces in a browser, `/dashboard` says what each panel is playing and changes it. Owns what plays where across restarts; see `docs/design/studio-vision.md`. |
| `docs/design/` | **Source of truth**: `protocol-v1.md`, `architecture.md`, `generative-art-brief.md`; `deployment.md` for running the studio as a service. |
| `docs/research/` | How we got here: stack choice, codec lab, transport, bring-up, end-to-end results. |
| `docs/board/` | Kanban: `backlog/`, `doing/`, `review/`, `done/`, `parked/`. See `docs/README.md`. |
| `lab/` | The codec measurement lab (frozen reference). |
| `tools/` | Bench scripts: flash + log + snapshot, camera daemon, flash backup, macOS signing. |

## Quick start

Host (stable Rust):

```bash
cargo run -p screeny-sim                         # a fake panel in a window
cargo run --release -p screeny -- discover       # find panels (real and simulated)
cargo run --release -p screeny -- clock          # word clock to the first panel found
cargo run --release -p screeny -- fractal --name screeny-sim
some-renderer | cargo run --release -p screeny -- pipe    # raw 6144-byte RGB888 frames on stdin
cargo test --workspace
```

`tools/sign-macos.sh` builds and Developer-ID-signs the `screeny` binary; use it for
anything launched outside a terminal, so macOS Local Network permission sticks.

Screeny Studio as a service, in a container, on a machine nobody logs in to:

```bash
docker compose -p screeny -f docker-compose.portable.yml up -d --build   # here, on a Mac
tools/deploy-workbench.sh --dry-run                                      # there, over SSH
```

`Dockerfile`, `docker-compose.yml` (Linux host + Intel iGPU),
`docker-compose.portable.yml` (everywhere else) and `tools/deploy-workbench.sh`.
The runbook, what to verify on the host and how to take it all away again are in
[`docs/design/deployment.md`](docs/design/deployment.md). **The studio has no
password**: on a LAN that is the point, but do not publish the port.

Firmware (needs `espup`'s Xtensa toolchain and `espflash`):

```bash
. ~/export-esp.sh
cd firmware && cargo build --release
tools/fw-run.sh firmware/target/xtensa-esp32-none-elf/release/screeny-fw boot 20   # flash, log, snapshot
```

WiFi credentials live in the device's settings partition, not in the firmware. Until
the captive portal lands (`docs/design/device-web.md`), seed a fresh device once with
`cargo build --release --features bench-wifi` (it embeds the pair from
`firmware/wifi.env` or `~/.config/screeny/wifi.env` and stores it on first boot); every
later default build joins from the store. The original Tidbyt firmware is backed up in
`backup/` with restore instructions.

## The protocol in one paragraph

One frame per UDP datagram, 8-byte header (magic, version/type, codec, flags, seq,
len) and at most 1464 bytes of pixels; newest frame wins, nothing is retransmitted,
every frame decodes on its own. Codecs: `PAL4_LZ` and `PAL8_LZ` (adaptive palette +
LZ, exact for low-colour and coherent images), `PAL5` (32 colours, always fits),
`BC1_DUAL` (4x4 blocks for photographic content), `SOLID`. The sender tries them per
frame and sends the best by a perceptual score against a model of the panel. Control
and telemetry ride a second port; discovery is DNS-SD `_screeny._udp`. Full spec:
`docs/design/protocol-v1.md`.
