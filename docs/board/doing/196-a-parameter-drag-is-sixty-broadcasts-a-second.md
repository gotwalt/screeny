---
id: 196
title: Dragging a parameter is sixty state broadcasts a second
type: build
hardware: no
depends: [120]
owner: worker-196
branch: card/196-state-broadcast-pacing
---

## Goal

Card 120's second, smaller case, left undone when the first was finished: the
frames a browser is sent are now paced to what it can use, and the *state* it is
sent is not.

## Context

Dragging a slider is about sixty `POST /api/v1/set_param` a second. Each one
persists (cheap - the store has a one-slot mailbox and the newest write wins)
and then broadcasts a full `StudioState` to every **other** browser, which is a
few hundred bytes of JSON each time. The browser that is dragging is skipped by
its `X-Studio-Client` id, so this only costs anything when a second browser is
open - and card 170 made that the normal case for a phone and a laptop on the
same page.

It is bounded: the broadcast is 32 deep and a listener that falls behind is sent
the current state instead of the ones it missed, so nothing queues without limit
and nothing can slow a player down. It is waste, not risk - the same shape as
card 120's frames, and the same answer would suit: the newest state is the only
one worth having, so send it at a rate rather than per change.

- `crates/studio/src/lib.rs`: `AppState::publish_state`, `STATE_BACKLOG`.
- `crates/studio/src/ws.rs`: the `states.recv()` arm, and `Pace`, which already
  does exactly this job for frames.
- `crates/studio/src/api.rs`: `set_param` and the other change routes.

One judgement to make first: a state message is also how a *deliberate* change
reaches the other browser (a piece picked, a switch flipped), and those should
feel instant. A rate cap of, say, 20 a second would be invisible to a human and
cut a drag by two thirds; coalescing by `rev` and only sending the newest would
be better still. Measure before deciding it is worth anything: with one browser
open it is already zero.

## Deliverables

- A measurement first: bytes/s to a second browser while a slider is dragged,
  which `sockets.bytes_sent` on `/api/v1/status` now makes easy.
- If it is worth doing, the state stream paced the way the frames are - and the
  page's own changes still arriving at once.

## Acceptance

Dragging a slider with two browsers open costs the second one a small, bounded
number of messages a second; a single change still reaches it immediately; the
existing "two browsers see each other's changes" test is untouched.

## Log

### The measurement, before anything was changed (worker-196)

`crates/studio/tests/pacing.rs`: a studio on an ephemeral loopback port, no
panel and no discovery, two WebSocket clients (`dragger` and `watcher`) both
asking `fps=0` so that what is weighed is state and the half-second heartbeat
and nothing else. `dragger` then posts `set_param hue` at 60 Hz for three
seconds with its own `X-Studio-Client` id, exactly as a slider under a mouse
does.

| | to the second browser |
|---|---|
| state messages | **180 in 3 s = 60.0/s** |
| bytes | **24 949 B/s** (416 B a message) |
| heartbeats | 6 (unaffected) |
| to the *dragging* browser | 0 state messages |

So the card's guess is exactly right: one change in, one full `StudioState`
out, per browser, sixty times a second, and 25 KB/s of JSON for one slider
moving. (A real page also has frames: at card 120's default 30 fps that is
another 186 KB/s, so the state is about an eighth of a dragging tab's traffic -
but it is an eighth that is pure waste, and on a hidden tab, which asks for no
frames at all, it is **all** of it: a hidden phone costs 0.5 KB/s at rest and
25 KB/s while somebody else drags a slider, a fiftyfold jump for something
nobody is looking at.)

**`sockets.bytes_sent` does count state messages**, not only frames: it is
incremented wherever a socket writes, so card 120's instrument serves this card
too. Cross-checked in the same run - the server counted 75 805 B written to all
sockets over the window against the 74 847 B the one browser received, the
difference being the other socket's heartbeats. The numbers above are
nevertheless measured **at the socket in the test client**, which is the
narrower instrument: it separates state from heartbeat, which the server's
counter does not.

Verdict: worth doing, and by the card's preferred shape.
