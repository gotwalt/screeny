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
