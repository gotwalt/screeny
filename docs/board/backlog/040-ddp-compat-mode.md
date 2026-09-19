---
id: 040
title: DDP secondary receive mode (xLights / LedFx interop)
type: build
hardware: no
depends: [001, 004, 008]
owner:
branch:
---

## Goal

Let the panel also act as a plain DDP display, so anything in the existing LED
ecosystem (xLights, LedFx, WLED-adjacent tooling, home-grown DDP senders) can
drive it with no knowledge of the screeny protocol.

## Context

Found while researching card 003; see `docs/research/003-protocol-transport.md`
§1.1 and §2.1. DDP is the only prior-art protocol with a header small enough to
be worth speaking (10 bytes, big-endian, UDP port 4048), and it already handles
multi-datagram frames natively via a 32-bit byte offset plus a `push` flag on
the final packet.

A 64x32 RGB888 frame is 6144 bytes = 5 DDP packets (4 x 1440 + 384). At 30 fps
that is 150 packets/s and ~184 kB/s, well inside what the ESP32 does, but it
needs a **6144-byte RGB888 reassembly buffer in SRAM**, which competes directly
with the HUB75 DMA framebuffer. That is why card 003 deferred it: the decision
is gated on card 001's memory budget.

The v1 spec was written so this can be bolted on: the display pipeline accepts
"here is a complete RGB888 frame" from a source that is not the screeny decoder,
and codec id `0x01` (`RGB888_RAW`) already exists.

## Questions to answer first

1. Is there 6 KB of SRAM to spare after card 001's budget? If not, close this
   card as won't-do and say so.
2. Does a DDP source integrate with the source arbitration in
   `docs/design/protocol-v1.md` §7, or does it need its own lock? (Proposal: it
   is just another source, identified by its UDP 4-tuple, with the same
   `LOCK_MS` rule. A partially reassembled frame from a source that loses the
   lock is discarded.)
3. What happens when a middle packet of a 5-packet frame is lost? (Proposal:
   drop the whole frame on the next `push` if any chunk is missing; count it in
   `frames_dropped_decode`. Do not display a torn frame.)
4. Do we answer DDP `Query` packets (id 251 status / 250 config, JSON bodies) so
   senders can discover the panel's geometry? Minimum is a `status` reply.

## Reference facts already established

DDP header, big-endian, from the spec at http://www.3waylabs.com/ddp/ and the
`ddp-rs` 1.3.0 implementation:

```
0      flags: bits 7:6 version (01), 0x10 timecode, 0x08 storage,
              0x04 reply, 0x02 query, 0x01 push
1      sequence number, low nibble, 1-15, 0 = unused
2      pixel config / data type
3      destination id: 1 default output, 246 control, 250 config,
              251 status, 254 DMX, 255 broadcast
4..8   byte offset into the display buffer, u32 BE
8..10  data length, u16 BE
10..14 optional timecode, u32 BE (WLED ignores it; we may too)
```

`MAX_DATA_LENGTH = 480 * 3 = 1440`. Port 4048.

## Deliverables

- DDP receive task in the firmware behind a cargo feature, default off.
- `crates/proto`: a `no_std` DDP header parser with tests.
- A short section in `docs/design/architecture.md` on how a DDP source and a
  screeny source share the panel.
- Interop evidence: a screenshot or camera capture of the panel driven by an
  off-the-shelf DDP sender.

## Acceptance

An unmodified third-party DDP sender configured for 2048 RGB pixels on port
4048 drives the panel at 30 fps, and the screeny protocol still works
unchanged when the DDP source stops.

## Log
