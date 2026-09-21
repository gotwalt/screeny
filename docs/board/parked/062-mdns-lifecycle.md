---
id: 062
title: mDNS lifecycle - goodbye on reboot, re-join after reconnect, TTL 255
type: build
hardware: yes
depends: [008]
---

## Goal

Finish the parts of protocol-v1 section 5.3 that card 008 did not implement, so a
sender's browse list is right within a second of the device going away or coming
back rather than within the 120 s record TTL.

## Context

Card 008 built the responder and the TXT record (`firmware/src/mdns.rs`): the
record is the `GET_INFO` body parsed back out, the host name is `screeny-<id>`
per section 5.1, and `SET_NAME` restarts the responder so it re-announces. What
section 5.3 asks for and card 008 did **not** do:

1. **A goodbye packet (TTL 0) on a clean shutdown or before rebooting.** The
   `REBOOT` opcode currently replies and resets; nothing tells the LAN. Card 008
   measured the device unreachable for ~11.4 s across a reboot, during which
   every browser still lists it.
2. **Re-joining `224.0.0.251` after a reconnect.** `edge-mdns`'s `io::bind` joins
   the group once, at task start. Whether the membership survives an esp-radio
   reassociation is untested; if it does not, discovery stops working after the
   first AP blip and nothing says so.
3. **`set_hop_limit(Some(255))`**, which RFC 6762 section 11 requires on mDNS
   sends. `edge-nal-embassy`'s `Udp` may not expose it; find out, and if it does
   not, say so here rather than leaving the requirement looking done.
4. **Re-announce on IP change**, not just on `SET_NAME`.

Nothing here was observable on card 008's bench, because the device never lost
its address except by rebooting, and macOS re-browsed each time.

## Deliverables

1. Goodbye before `REBOOT` and before card 014's `SET_WIFI` reconnect.
2. Multicast re-join and a fresh announcement burst (RFC 6762 section 8.3: 2-8
   announcements, first pair a second apart) on link-up and on address change.
3. Hop limit 255 on the mDNS socket, or a written finding that the stack will
   not do it and what that costs.

## Acceptance

`dns-sd -B _screeny._udp` on this Mac shows the device disappear within a second
of a `REBOOT` and reappear when it returns, and still resolves after the AP has
been power-cycled once.

## Log

- 2026-09-21, firmware orchestrator: **parked** with the owner's agreement, under decision 10 of `docs/design/device-web.md` ("not a commercial product, we don't need to overly bomb-proof it"). A read-only survey against fw 0.7.0 found it still open as written and not a crash or memory risk. Not picked up without the owner asking.
