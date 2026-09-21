---
id: 169
title: flock - the camera cannot ride close without scattering the flock
type: build
hardware: no
depends: [168]
owner:
branch:
---

## Goal

In `flock` (card 168), `near` - "how close the camera rides" - does two jobs at once,
and they fight. It sets the camera's **seat** (how far behind the flock it sits) and
its **personal space** (the separation radius at which it pushes birds away, `near *
1.15`, at a weight of 7.0 against a bird's 3.4). Turn it down and the camera does ride
closer, which is the better picture - at `near` 2.5 a foreground bird is 8-10 LEDs and
unmistakable, where at the default 6.0 it is about 6 - but the camera is then sitting
*inside* the flock as a strong repulsor and blows it apart.

Measured, three seeds x ten minutes, default parameters except `near`:

| `near` | flock spread (median) | camera's seat, p95 | birds in frame, worst |
|---|---|---|---|
| 6.0 | 4.8 m | 14.2 m | 17 |
| 4.5 | 14.4 m | 57.9 m | 0 |

So the default is 6.0 and the best-looking value is not available. That is the bug.

## Context

- `crates/art/src/patches/flock/sim.rs`, `Sim::steer`: `sep_r` and `keep` for the
  camera arm, and the `seat` a few lines below.
- Card 168's Log and the strips `flock-5-close.png` (near 2.5) and `flock-1-light-level.png`
  (near 6.0) in that card's scratchpad show what is being traded away.
- The card 168 brief says the camera is a boid and is in the flock's neighbour lists,
  which is worth keeping: it is why the flock parts around it and why the seat feels
  earned rather than scripted.

## Ideas worth trying, in order

1. **Separate the two meanings.** `near` keeps the seat; the camera's personal space
   becomes its own thing, held at a value that does not disturb the flock (or scaled
   with the flock's own separation rather than with `near`).
2. **Make the camera's push asymmetric.** It avoids birds hard; birds treat it as an
   ordinary neighbour. This keeps "it is in their lists and they are in its" true for
   alignment and cohesion, and only the separation *weight* differs by direction. Say
   so in the module docs if it is done - it is the kind of quiet asymmetry that is a
   lie if it is not written down.
3. **Let it ride ahead of a flank rather than inside.** A seat offset to the side and
   slightly forward, looking back along the flock, gives near birds without the camera
   being in the middle of anything.

## Acceptance

At `near` 2.5-3.0, over the same three seeds x ten minutes: flock spread stays under
7 m, the camera's seat p95 stays under 22 m, and at worst 15 birds are in frame -
the bounds `ten_minutes_of_flight` already asserts. Plus a strip showing the 8-10 LED
foreground birds that motivated it.

## Log

### Closed on card 177's evidence (orchestrator, 2026-09-20)

Card 177 did not aim at this and fixed it: the flock now holds together for reasons that
do not need stiff separation. At `near 3.0`, three seeds x ten minutes: spread 4.8-6.0 m
(asked < 7), seat p95 12.4-13.4 m (asked < 22), 28-36 birds in frame at worst (asked >=
15), foreground birds 8-10 LEDs. The default `near` stays 6.0; which picture he wants is
the owner's.
