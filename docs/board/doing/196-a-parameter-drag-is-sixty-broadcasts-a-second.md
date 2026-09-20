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
