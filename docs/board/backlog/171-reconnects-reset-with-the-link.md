---
id: 171
title: The page's "Reconnects" count resets whenever the link is rebuilt
type: build
hardware: no
depends: [170]
owner:
branch:
---

## Goal

The panel section shows **Reconnects**, and the number is not what a person reads it
as. It is `LinkStats::sessions - 1`, which is per *link object*, and the studio builds
a new link whenever what it is aiming at changes - a device that is re-resolved after
a stale period, a panel that moved, panel output switched off and on again. So a panel
that really has reconnected three times can show `0`, and one that has never dropped
can show `1`.

Seen while rendering card 170: the simulator was unplugged and plugged back in, the
API reported `sessions - 1 == 1` for a moment, and then the registry re-resolved the
device, the link was rebuilt, and the page settled on `Reconnects 0`.

Card 106 noticed the same thing from the other end and wrote it down in its Log as an
explanation for a surprising soak number ("`LinkStats` is per link, and the link is
rebuilt every time the panel *moves*"). Card 170's acceptance is that there is **no
control or readout on the page whose meaning is unclear**, so it is now a bug rather
than a footnote.

## Context

- `crates/studio/src/player.rs`: `LinkSlot.sessions` and `PlayerHealth.sessions`.
  `supervise()` copies the link's count into the player's every second; `aim()` sets
  `slot.sessions = 0` when it rebuilds.
- The honest number is a *player* lifetime count: "how many times has this panel's
  stream come up since the studio started", carried across link rebuilds the way
  `ticks` is carried across core restarts (card 106's bug 5, same shape).
- Worth telling apart, if it is cheap: a reconnect the *panel* caused (it went away)
  from one the *studio* caused (it re-aimed). Only the first is interesting to a human.

## Deliverables

- A player-lifetime reconnect count that survives a link rebuild.
- The page shows that, and says "since the studio started" if the label needs it.
- A test: a panel that goes away and comes back twice reads `2`, including when the
  registry re-resolves it in between.

## Acceptance

Unplug the panel and plug it back in three times; the page says 3.

## Log
