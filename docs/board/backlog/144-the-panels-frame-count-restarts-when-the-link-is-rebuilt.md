---
id: 144
title: The panel's frame count restarts at zero when the link is rebuilt
type: build
hardware: no
depends: [171]
owner:
branch:
---

## Goal

`frames_sent` on the page is the *link's* lifetime count, and the studio builds a new link
whenever the way to reach a panel changes. The number a human is watching then drops to
zero although nothing about the panel changed. Decide whether that is what the page should
show, and if not, carry it across a rebuild the way card 171 carried `reconnects`.

## Context

Found while fixing card 117's soak flake, and it was the *cause* of that flake: the soak
waited for `panel.frames_sent > before + 10` and the counter went backwards underneath it.
Measured, from the soak's own output, three times in a 60 s run - once per "the panel
moves" round, and twice within the round: a typed address becomes `Reach::Addr` and then,
seconds later, `Reach::Resolved`, and `Player::aim` opens a new `Link` for each.

```
soak: after the panel moving to another address the panel's frame counter went backwards,
      43 -> 1: the link was rebuilt
```

Card 171 already did this for `reconnects`, which is carried across a link rebuild
"the way `ticks` is carried", precisely so a rebuild does not read as the panel having
gone away. `frames_sent`, `frames_offered`, `bytes` and the two indexed counters were not
part of that card, and are still per-link.

This is not a fault - the counters are honest about what they count - but the page's stats
strip presents them as "this panel, since the studio started", and a number that restarts
is a number a person has to know the implementation to read. It also lays a trap for any
future test or UI that watches it go up.

## Deliverables

- A decision, written down: either the page's frame counters are player-lifetime (carried
  across a rebuild, like `reconnects`), or they are per-link and the page says so.
- Whichever it is, implemented in `crates/studio/src/player.rs` (and the page, if it is
  the second) and covered by a test that rebuilds a link and reads the counter.

## Acceptance

Move a panel to another address while it is playing: the page's frame count does what the
decision says it should, and the soak's `flowing()` re-base counter (card 117) can say
whether a rebuild still moves it.

## Log
