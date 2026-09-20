---
id: 132
title: The two oversize rules cannot be observed over WiFi; say so in the spec
type: design
hardware: no
depends: [080]
owner:
branch:
---

## Goal

Add a note to `docs/design/protocol-v1.md` saying that sections 1 and 2.3's
oversize rules are not testable across a real link, so that nobody reads a
quiet counter as a pass.

## Context

Found while writing card 080's conformance suite.

Two MUSTs are about datagrams that are too big:

- **Section 1**: "A receiver's frame buffer is 1472 bytes... A receiver MUST
  discard such a datagram and count it in `frames_rejected` rather than parse
  the truncated prefix."
- **Section 2.3**: "For a `FRAME`, `len` MUST be `<= 1464`", which a receiver
  counts in `frames_rejected` too.

Both need a UDP payload over 1472 bytes to reach the device. Over 802.11 it
cannot: the IP packet exceeds a 1500-byte MTU and is fragmented, and section 1
already says the device does not reassemble fragments
(`ipv4-reassembly` is off), so the datagram is dropped in the stack and
**counted nowhere**. A sender that sets `IP_DONTFRAG` as section 9.2 advises
cannot even transmit it.

So on hardware a conformance probe for either rule sees `frames_rejected == 0`
- which is what a *violation* looks like as well. The suite marks both rules
`LOOPBACK_ONLY` and prints `SKIP  loopback only: the radio fragments it away`
rather than a misleading pass, and `crates/sim` keeps the real assertions
(`malformed.rs`, where the `Reject::TooLong` reason is visible in process).

This is not a bug in either implementation. It is a property of the rule that
the spec should state, next to the rules themselves, so the next person to
write a device-side test does not spend an afternoon on it.

## Deliverables

- A paragraph in section 1 and a sentence in section 2.3 of
  `docs/design/protocol-v1.md`: these two are verifiable only over loopback or
  a link whose MTU exceeds 1500, and the simulator is where they are tested.

## Acceptance

The spec says it, and `docs/research/` is not the only place the reasoning
exists.

## Log
