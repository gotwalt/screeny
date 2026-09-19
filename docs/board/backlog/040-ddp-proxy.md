---
id: 040
title: DDP proxy in the sender (host-side DDP -> native bridge)
type: build
hardware: no
depends: [009]
owner:
branch:
---

## Goal

Let existing DDP software (LedFx, xLights, Hyperion, WLED sync) drive the panel
without the firmware knowing DDP exists. A host-side proxy listens for DDP,
reassembles full frames, and forwards them to the device as native single-datagram
frames using the normal sender pipeline.

## Decision record

Card 003 proposed DDP as a secondary receive mode *in firmware*. The owner proposed
moving it into the sender instead (2026-09-19) and that is the plan:

- A 64x32 RGB888 DDP frame is 6144 bytes = 5 datagrams. On the WiFi hop that means
  5x packet count, frame loss amplified ~5x (any lost fragment spoils the frame, and
  a lost PUSH packet leaves the frame unterminated), a burst that fills the WiFi
  driver's 5-deep rx queue, and reassembly buffers in scarce SRAM.
- In a proxy the 5-packet reassembly happens on loopback / wired LAN where loss is
  ~0, and the lossy hop carries only native frames. Firmware keeps one receive path.
- DDP sources inherit our codec selection and dithering for free.
- Cost: a host process must be running; the panel is not a standalone DDP device.
  Accepted. Firmware-side DDP is dropped from the roadmap, not deferred.

## Context

- DDP spec: http://www.3waylabs.com/ddp/ . 10-byte header (flags incl. PUSH and
  optional 4-byte timecode, sequence nibble, data type, destination id, u32 data
  offset, u16 length), default UDP port 4048, RGB888 payload, big-endian fields.
- Builds on the sender library from card 009 (frame in -> encode -> pace -> send).
- Protocol spec: `docs/design/protocol-v1.md`.

## Deliverables

- `screeny ddp-proxy [--listen 127.0.0.1:4048] [--device NAME|--addr IP]` subcommand
  in the sender CLI:
  - parse DDP data packets (honour offset/length, ignore query/reply/config packets,
    tolerate the timecode flag); write into a 64x32 RGB888 staging buffer; bounds-check
    everything (offset + length <= 6144);
  - on PUSH, mark the staging buffer complete. If a source never sets PUSH, treat
    "offset wrapped back to 0" or a 10 ms gap as end of frame;
  - resample to the device rate: on each 33.3 ms tick send the newest complete frame,
    skip if nothing new (device holds last frame); never burst to catch up;
  - default listen address is loopback; binding to a LAN address is opt-in.
- Unit tests with synthetic DDP streams: in-order, reordered fragments, missing PUSH,
  source faster and slower than 30 fps, oversize offset.
- README section: how to add the proxy to LedFx as a DDP device (2048 pixels, 64x32
  matrix, RGB order).

## Acceptance

LedFx (or a scripted DDP source) pointed at the proxy drives the simulator at a
steady 30 fps with no tearing, and the device-side telemetry shows one native packet
per displayed frame.

## Log
