---
id: 171
title: The page's "Reconnects" count resets whenever the link is rebuilt
type: build
hardware: no
depends: [170]
owner: worker-173
branch: card/studio-page-tells-the-truth
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

### The card against today's tree

Unchanged. `LinkSlot.sessions`, `PlayerHealth.sessions`, `supervise()` copying
one into the other and `aim()` setting it back to zero are all still there, and
`ui/main.js` still drew `Math.max(0, player.health.sessions - 1)`.

One extra thing the card did not mention, found on the way: `configure()` also
set `slot.sessions = 0` when the brightness policy changed, purely to make the
supervisor think a new session had opened so it would re-apply the policy. That
is a second reason the number was wrong, and once reconnects were counted it
would have *invented* one. It is an explicit `reapply_brightness` flag now.

### The decision

**Kept, not deleted** - the card 170 worker asked. It can be right, and a panel
that has dropped six times overnight is worth knowing about.

Two numbers, because one cannot carry both facts honestly:

- `link_ups` - every time this panel's stream has come up since the studio
  started, carried across link rebuilds the way `ticks` is carried across core
  restarts. The raw, unarguable count.
- `reconnects` - `link_ups` less the first connect and less the ones the
  **studio** caused, which is the card's "worth telling apart, if it is cheap".
  It was cheap: `aim()` already knows when it is the reason, so it is one
  `studio_ups += 1` in the two places where that is true (see "What the
  browser caught" below for the second of them, which I did not see coming).

A link rebuilt from one *resolved* device to another - a panel that moved, or
one re-resolved after a stale period - is deliberately **not** discounted: the
panel really was away and really did come back, which is what the reader wants
to know.

`health.sessions` stays, unchanged, as the per-link number it always was, with
a doc comment that now says so - it is still the right thing for "is this one
link flapping".

### What I did

- `player.rs`: `LinkSlot` gains `closed_ups` (banked from closed links),
  `studio_ups`, `from_resolved` and `reapply_brightness`;
  `LinkSlot::close_link` banks and discounts in one place; `supervise()`
  computes `link_ups = closed_ups + sessions` and
  `reconnects = link_ups - 1 - studio_ups`.
- `PlayerHealth` gains `link_ups` and `reconnects` (additive; `sessions` kept).
- The page: `Reconnects  2 since the studio started`. The label says the window
  because without it the number has no meaning.
- `tests/soak.rs` prints `reconnects` instead of `sessions - 1`, which is the
  number card 106's Log went looking for and could not trust.

### Evidence

`tests/panel.rs::a_panel_that_comes_back_twice_says_two` (12 s): a simulator on
a known port pair is dropped and a fresh one started in its place, twice; the
page says 2, and `link_ups` is at least one higher per round. Then
`set_panel {"on":false}` / `{"on":true}` - which **rebuilds the link**, the
exact thing that used to wipe the count to 0 - and it still says 2, while
`link_ups` goes up again so nothing is hidden. Every wait is condition-based
with a 30 s deadline and every assertion is made on the read that satisfied
its wait; only `reconnects` is asserted exactly, because it is the number the
card is about and the only one the studio's own re-aiming cannot move.

### What the browser caught that the test did not

Rendering it against a loopback simulator, a **freshly attached panel read
"Reconnects 1"** before anybody had touched it. Adding a panel by address
aims the link at the address; seconds later the telemetry poll finds out which
device is there, the reach becomes `Resolved`, and `aim()` rebuilds the link.
The panel had not moved an inch, and my first rule counted it.

So `studio_ups` has a second case: **the studio learning where the device
really is**, one non-resolved reach to a resolved one. A rebuild from one
*resolved* device to another is still a genuine reconnect - that panel was
away.

And a second bug under it, which the first fix made visible: discounting a
rebuild is only right when the link being replaced had **actually come up**.
Replacing a link that never connected costs no extra session, so discounting
there hid a real reconnect - which is exactly what happened when the address
resolved before the first link had finished connecting. `LinkSlot::close_link`
now banks and discounts in one place.

The integration test was rewritten to count in deltas from a baseline, so it
does not depend on how many link-ups the attach itself took.

### Rendered

- `docs/research/img/18x-171-reconnects.png` - the panel section after the
  simulator was stopped and started again once: **"Reconnects  1 since the
  studio started"**, beside `Link up · 30 fps` and `Heard 2 s ago`.
- Before that, freshly attached and resolved, `/api/v1/status` read
  `reconnects: 0, link_ups: 2` and the page said `0 since the studio started`.

### And a third, from running it six times

The card's own footnote - "worth telling apart, **if it is cheap**" - turned
out to be the load-bearing word. Telling a studio-caused link rebuild from a
panel-caused one is cheap only if the two cannot overlap, and they can. Three
passes, each found by running rather than by reading:

1. **A freshly attached panel read 1.** Fixed by booking "the studio learned
   where the device is" to `studio_ups` (found in the browser).
2. **A real reconnect read 0.** Discounting a rebuild is only right when the
   link replaced was up; replacing a link that never connected consumes no
   session. Fixed by the `is_up()` condition.
3. **`slot.sessions` is the supervisor's once-a-second copy.** A link built and
   torn down between two ticks banked a zero it had not earned, so the
   discount ran without the credit. `close_link` now reads the closing link's
   own `stats().sessions`.

**What is left, deliberately.** If the studio re-aims while the stream is up
*and* the panel goes away before the replacement connects, the panel's return
is the same connect as the re-aim landing, and it is not counted. Under-
counting in a rare overlap is the right side to err on - the alternative is
telling somebody their panel dropped when it did not - and `link_ups` still
counts it, so nothing is hidden. Chasing it further would mean modelling
*why* a link is down, which is not cheap, and the card said not to.

The test now waits for the count to **stop moving** before its baseline, which
is a condition and not a fixed sleep: the studio settling on a freshly typed
address is exactly the overlap above, and a test must not start a round inside
it. Before that wait, three of five `--release` runs failed; after it, six of
six passed, at ~13 s each.
