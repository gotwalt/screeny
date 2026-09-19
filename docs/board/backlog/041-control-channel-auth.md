---
id: 041
title: Authenticate mutating control ops for untrusted networks
type: design
hardware: no
depends: [004, 008, 014]
owner:
branch:
---

## Goal

Design (and, if cheap, build) authentication for the control ops that can
actually hurt - `SET_WIFI` and `REBOOT` - so the panel can go on a network the
owner does not control.

## Context

Found while researching card 003; see `docs/research/003-protocol-transport.md`
§1.9 and `docs/design/protocol-v1-draft.md` §8.4. v1 is **deliberately**
unauthenticated: the owner has stated the Wi-Fi password is not a secret and
the LAN is trusted, and card 003 was explicitly told not to over-engineer this.
This card exists so the reasoning is not rediscovered from scratch later, not
because v1 is wrong.

Threat model, in order of seriousness:

1. `SET_WIFI` from a stranger - moves the panel onto an attacker's network, and
   hands the attacker the real network's PSK when the device later tries to
   rejoin. This is the only one that matters.
2. `REBOOT` - denial of service, annoying.
3. Seizing the frame stream - vandalism. The `LOCK_MS` arbitration rule already
   bounds this to an annoyance, and authenticating 30 packets/s on an ESP32 is
   not worth it.

## Options, cheapest first

1. **Build-time disable.** A cargo feature that compiles out `SET_WIFI` and
   `REBOOT` entirely; they return `ERR_NOT_PERMITTED`. Zero crypto, zero RAM.
   Provisioning then only happens over serial. This may be the right answer.
2. **Panel PIN + replay counter.** A 32-bit PIN shown on the panel by
   `IDENTIFY`. Mutating control packets carry the PIN and a monotonic counter
   the device persists; the device rejects a counter it has already seen. About
   a day of work. Stops a stranger who cannot see the panel; does not stop
   someone sniffing the LAN.
3. **PIN-authenticated key exchange.** SPAKE2 over the PIN, yielding a
   ChaCha20-Poly1305 session key for the control channel.
   `chacha20poly1305` builds `no_std`; a SPAKE2 implementation needs checking
   for `no_std` support and code size on Xtensa. Days of work, properly secure.

## Invariants that must hold in every version, including v1

- The PSK is never returned by `GET_WIFI`, never appears in telemetry, and is
  never shown on the panel or printed by the serial console. Verify this holds
  in the shipped firmware as part of this card.

## Deliverables

- `docs/design/control-auth.md`: threat model, the chosen option, and why the
  others were rejected.
- Whichever of the options above is chosen, implemented, plus tests for the
  replay/nonce logic if applicable.
- A check (test or code review note) that the PSK-never-leaves invariant holds.

## Acceptance

The orchestrator can decide whether to deploy the panel on a network they do not
control, and the firmware behaves as the document says it does.

## Log
