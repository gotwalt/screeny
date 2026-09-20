---
id: 164
title: How much network a panel costs - KB/s out and in, on the Panel screen
type: build
hardware: no
depends: [198, 180]
owner:
branch:
---

## Goal

The owner, 2026-09-20: "i'd like a KB/s tracker per panel to understand how much network
traffic we're sending (and receiving, during the http calls i assume)."

For each panel, the Studio counts every byte it sends to it and every byte it gets back,
over every path, and the Panel screen shows the rates.

## Context

- The paths between the Studio and one panel, all of which already go through a few
  places:
  1. **Frames**, UDP, Studio -> panel: `screeny::Link` inside the player
     (`crates/studio/src/player.rs`). The link's status already reports the last frame's
     `bytes` and `frames_sent`; it does not keep a running byte total. About 30 datagrams
     a second of 0.4-1.4 KB: this is nearly all of the traffic.
  2. **Control and telemetry**, UDP, both ways: `screeny::ControlClient` via
     `crates/studio/src/api.rs::on_device` (brightness, identify, name, reboot, stats) and
     the fleet's telemetry poll in `crates/studio/src/fleet.rs`. Requests out, replies and
     `TELEMETRY` datagrams in. Also whatever the link itself receives on its socket
     (`BUSY`, acks if any - read `crates/screeny/src/link.rs` / `sender.rs`).
  3. **HTTP status**, TCP, both ways: `crates/studio/src/devhttp.rs::get_status`, every
     10 s (and card 199's `/api/v1/panic` later): request bytes out, reply bytes in.
  4. mDNS browsing and the broadcast probe (card 141) are not traffic *with a panel*;
     leave them out and say so.
- Count **payload bytes plus a stated per-packet overhead** so the number means something
  on the wire: UDP datagrams are payload + 28 bytes (IPv4 + UDP headers); for HTTP count
  the bytes written and read on the socket and add nothing (TCP/IP overhead and ACKs are
  not visible from user space - say so in the tooltip/README rather than guessing). Keep
  both raw counters (`bytes`, `packets`, per direction, per path) in `/api/v1/status`
  under the device, and let the page compute the rate.
- Rates: an exponentially weighted or a sliding ~5 s window, computed in **one** place
  (server side is better: every browser then shows the same number, and the state is
  bounded - a few u64 per device). KB = 1000 bytes; show `KB/s`, one decimal under 10.
- Where it shows: the Panel screen's Link block (card 198, `panel.html` / `panel.js`):
  one line, e.g. `Network  36.4 KB/s out · 0.3 KB/s in`, with the split by path in the
  `title` or a second quiet line (`frames 36.1 · control 0.1 · http 0.2`). Nothing on the
  Picture screen. No chart - a number; if a chart is wanted later that is another card.
- Counters must not cost the render or send path anything measurable: atomics or a field
  the sender already owns; no locks on the frame path; no per-frame logging.
- Totals since the Studio started are worth keeping beside the rates ("2.1 GB out since
  Tuesday" is the number that answers "what does this cost my network").
- `crates/screeny` is shared with the `screeny` CLI: adding a running byte total to the
  link's/sender's stats is additive and fine; do not change its behaviour or the wire.

## Deliverables

- Byte and packet counters per path and direction (`crates/screeny` link stats as needed;
  `devhttp.rs`; the control path; the telemetry poll), kept per device in
  `crates/studio/src/devices.rs`, reported in `/api/v1/status`.
- The rate, computed once; the line on the Panel screen; `tests/ui.rs` id checks.
- Tests against `screeny-sim` on loopback: streaming a known patch for a few seconds gives
  a frames-out rate within 10% of `frames_sent x mean bytes`; an HTTP poll moves the http
  counters both ways; a device that is away counts nothing.
- README: what is counted and what is not.

## Acceptance

On the deployed Studio's `/panel`: a KB/s out that agrees with `fps x frame size` from the
same page, a small KB/s in, and totals that only grow.

## Log
