---
id: 003
title: Wire protocol, discovery and transport research
type: research
hardware: no
depends: []
owner: worker (agent-abe43d01)
branch: card/003-protocol-transport
---

## Goal

Propose the v1 network protocol for streaming frames to the device: datagram layout,
discovery, control/telemetry, and sender pacing, informed by prior art and by how
ESP32 WiFi actually behaves.

## Context

- One frame per UDP datagram, <= 1472 bytes payload, 30 fps target, newest-wins,
  no retransmit. Pixel encoding is being researched separately (card 002); leave a
  codec-id byte and treat the pixel payload as opaque.
- Firmware will be `no_std` embassy-net (smoltcp) on ESP32 (card 001).
- Host side is Rust on macOS (should also work on Linux).
- Wanted: mDNS / DNS-SD discovery so senders find the panel by name with no config.
- Wanted later: change WiFi credentials at runtime (no reflash).
- We need on-device telemetry because the camera cannot measure > 30 fps or latency:
  frames received / shown / dropped, inter-arrival jitter, decode time, RSSI, uptime.

## Questions to answer

1. Prior art worth copying or being compatible with: WLED realtime UDP (DRGB/DNRGB/
   DDP), Art-Net, E1.31/sACN, Pixelflut, tidbyt community firmwares. Is there value in
   speaking DDP as a secondary raw mode? (A raw 64x32 RGB888 frame does not fit one
   packet, so our native format is primary regardless.)
2. Datagram header: magic/version, codec id, sequence number, flags (keyframe etc.),
   timestamp or frame duration? Keep it tiny; justify each byte.
3. ESP32 WiFi UDP receive behaviour: realistic latency and jitter on a home 2.4 GHz
   network, effect of modem power save (must disable?), AMPDU/aggregation, burst loss,
   broadcast/multicast vs unicast delivery reliability, what sender pacing avoids
   drops. Cite measurements where they exist.
4. Jitter handling on device: show immediately vs. a 1-2 frame playout buffer;
   recommendation for a visualizer (latency-sensitive) use case.
5. Discovery: DNS-SD service type name (e.g. `_screeny._udp`), TXT records (width,
   height, codecs supported, protocol version, name), host-side Rust crates for
   browsing (`mdns-sd` etc.) and macOS quirks (local network permission prompts).
6. Control + telemetry channel: same UDP port with packet types, or a second port?
   Request/response for stats, brightness, identify, reboot, set-wifi. How the sender
   learns it is overrunning the device.
7. Runtime WiFi provisioning options ranked by effort: serial console command,
   authenticated UDP control packet, SoftAP fallback with captive page, BLE, Improv
   WiFi (serial/BLE standard). Recommend a v1 and a later path.
8. Multiple senders: arbitration rule (last writer wins vs. lock to a source for N
   ms) and idle behaviour when the stream stops (hold / fade / show status screen).

## Deliverables

- `docs/research/003-protocol-transport.md`: conclusions first, evidence with links.
- `docs/design/protocol-v1-draft.md`: a concrete draft spec - byte-level packet
  layouts, port numbers, DNS-SD names and TXT keys, state machine for source
  arbitration and idle timeout, telemetry fields. Mark open questions explicitly.

## Acceptance

The orchestrator can finalise the spec by filling in the codec table from card 002,
and two independent workers could implement sender and receiver from it and
interoperate.

## Log

### 2026-09-19 - worker, branch `card/003-protocol-transport`

Documents only, no hardware, no code. Delivered
`docs/research/003-protocol-transport.md` (conclusions first, evidence with
links) and `docs/design/protocol-v1-draft.md` (byte-level, implementable).

**What was decided**

- **Own protocol, not an existing one.** No LED protocol in the wild carries a
  codec id, and that byte is the whole point of this project. DDP, WLED DRGB,
  Art-Net and sACN all assume raw RGB and all need 5-13 packets for a 64x32
  frame. Borrowed DDP's tiny-header discipline, its `push`/`FINAL` semantics and
  its split of pixel data from control/status messages.
- **8-byte frame header**: magic `0x53`, version+type nibbles, codec, flags
  (`KEY`, `STATS_REQ`, `FINAL`, `HAS_TS`), `u16` seq, `u16` len. 1464 bytes of
  pixel payload. No timestamp (jitter is differential and needs no clock sync;
  latency comes from `PING`), no source id (the UDP 4-tuple is free).
- **Two ports**: frames 49374, control 49375, both in the dynamic range, both
  published via DNS-SD.
- `_screeny._udp.local.`, TXT keys `txtvers proto w h codecs mtu ctrl fw id
  name`. `GET_INFO` returns the *same bytes* in DNS-SD TXT wire format, so
  there is one schema and one parser whether you browsed mDNS or were handed an
  IP.
- 48-byte telemetry struct with drops split four ways by cause, which is what
  lets a sender tell "network-limited" from "decode-limited" - opposite fixes.
- Last-writer-wins with a 500 ms lock, `BUSY` notification to the loser; hold
  the last frame 10 s then cross-fade to a status screen. Never blank to black.
- Provisioning v1: serial command + `SET_WIFI` control op, both writing
  `sequential-storage`, with Improv Serial as the cheap v1.1 upgrade and SoftAP
  captive portal as v2.

**Surprises worth passing on**

- **embassy-net 0.9.1 no longer uses smoltcp.** It depends on `xarxa` -
  dirbaio's rewrite, single global packet pool, zero-copy - pulled as a *git*
  dependency pinned to a rev, not from crates.io. Feature names moved with it.
  It also has **no default features at all**, so `multicast` and
  `ipv4-reassembly` are both opt-in. Card 001 should know this before writing a
  `Cargo.toml`.
- **esp-radio 0.18's `PowerSaveMode::default()` is `None`**, and `wifi::new()`
  applies it, so bare-metal Rust starts with modem sleep already off - the
  opposite of ESP-IDF's `WIFI_PS_MIN_MODEM` default. The action item is "assert
  it stays off", not "remember to disable it". With it on, measured ping RTTs
  blow out from 2.4 ms to 2278 ms (esp-idf#9766).
- `esp_radio::wifi::ControllerConfig` defaults already queue `rx_queue_size: 5`
  frames (166 ms at 30 fps) below the socket, with AMPDU RX on. That is why the
  device must drain-and-discard rather than buffer, and why a sender must never
  burst to catch up.
- Best public ESP32 numbers on a real 2.4 GHz network: **median ~9-10 ms RTT,
  outliers ~25 ms** (Electric UI). Loss is bursty and recovery is a burst of
  ~10 packets (esp-idf#15345), not a smooth catch-up.
- **macOS CLI tools run from Terminal or SSH are exempt** from the Local Network
  permission prompt, so `cargo run` just works - but closing Terminal while a
  child is running revokes the exemption mid-run, and there is an acknowledged
  kernel bug that caches a *denial* after reboot so multicast silently vanishes.
  Hence: a browse that finds nothing must be retriable and `--addr` must always
  work.
- Nothing to interop with on the Tidbyt side: stock and Tronbyt firmwares fetch
  animated WebP over HTTPS polling or a WebSocket, with no LAN streaming and no
  mDNS. Their SoftAP config portal at 10.10.0.1 is the reference for
  provisioning option 4.

**Recommendation the card asked for but is easy to miss**: no playout buffer.
Show immediately. The panel's DMA refresh is far above 30 Hz, so a late frame
just repeats the previous one for a few refresh cycles - visually identical to
what a 1-frame buffer would produce, at 33 ms less latency.

**New cards filed**: 040 (DDP secondary receive mode, gated on card 001's SRAM
budget), 041 (control channel auth for untrusted LANs - explicitly *not* v1,
recorded so the reasoning is not redone).

**Open questions** are listed at the end of both documents; the blocking one is
the codec table from card 002.
