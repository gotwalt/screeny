---
id: 106
title: Studio players, devices and state - built to be forgotten
type: build
hardware: no
depends: [105]
owner:
branch:
---

## Goal

The server owns what plays on which panel and keeps doing it for months with nobody
watching. Read "Built to be forgotten" and "Decisions" in
`docs/design/studio-vision.md`; they are the requirements.

## Deliverables

- **Device registry**: mDNS browse (`crates/screeny` discover) merged with manually
  configured addresses; devices keyed by their stable id (`id=` TXT / `GET_INFO`), not
  by IP. A *collection* everywhere (API, state file, code) even though one panel is
  the expected case; no multi-panel UI, sync or fan-out work (card 091 stays parked).
- **Players**: one per device: piece + params + seed + fps + brightness policy,
  rendering through the art pipeline into `screeny::Sender` (exact indexed frames,
  auto-reconnect from card 011). A distinct *preview* player backs the design view and
  can be pointed at a device or not; "make this what panel X plays" is explicit.
- **Containment**: a piece that panics or stalls is caught (`catch_unwind` + a
  watchdog on frame production), logged once, replaced by a safe fallback; the process
  and the other players are unaffected.
- **State store**: one small file on a volume, written atomically (temp + rename),
  versioned schema; resume exactly after restart. Corrupt/missing state -> sane
  default (first device found plays a default piece), never a crash loop.
- **Health**: `GET /healthz` (200/503) and `GET /api/v1/status`: per device last frame
  sent, last telemetry heard, fps, drops by cause, RSSI, uptime, reconnect count.
- **Device controls** in the UI via the control client: brightness, identify, name,
  stats, reboot.
- **Soak**, automated and bounded: a test binary runs the server against
  `screeny-sim` with fault injection (drops, restarts of the sim, address change) at
  accelerated time for a fixed duration and asserts flat memory, no task death, and
  recovery after every fault. Hours of wall-clock soak on workbench happen after 107,
  watched through `/healthz`, not by a worker sitting in a loop.

## Acceptance

Kill and restart the simulator, the server, or both, in any order: the panel comes
back showing what it was showing, with no operator action, every time.

## Log
