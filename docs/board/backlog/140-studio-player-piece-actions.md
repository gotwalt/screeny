---
id: 140
title: A device player's composing pieces cannot be acted on
type: build
hardware: no
depends: [106]
owner:
branch:
---

## Goal

`Piece::playing()` offers actions - the clocks' "Play it again" and "Compose another" -
and the dashboard shows what a panel is performing but cannot press them.
`Player::act` is deliberately a no-op (card 106).

## Context

Reaching into a running piece from another thread means taking a lock that the render
path holds, which is the thing card 105's handover said to avoid and card 106 was
careful to keep out of the device players. The honest shape is a small mailbox the
render loop drains between frames - one slot, newest wins, like everything else in this
server - rather than a lock around the piece.

## Deliverables

- A one-slot action mailbox on `Player`, drained by the render loop before each frame.
- `POST /api/v1/player/act {device, action}`, and the actions on the dashboard card
  next to what the panel is performing.
- A test: an action taken through the API changes what a device player is performing,
  and the render loop's frame rate is unaffected by a burst of them.

## Acceptance

From a phone, "Compose another" on the panel that is playing the clocks.

## Log
