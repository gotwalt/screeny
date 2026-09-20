---
id: 120
title: Preview bandwidth - pace the WebSocket to what a browser can use
type: build
hardware: no
depends: [105]
owner:
branch:
---

## Goal

A browser watching the studio should be sent as many preview frames as it can draw,
and no more. Today it is sent all of them.

## Context

Card 105 made the preview a WebSocket push (`crates/studio/src/ws.rs`). Every open
socket gets every frame the attached panel's player makes: **6196 bytes at 60 fps =
372 KB/s per tab**, whether or not the tab is visible, and whether or not it can draw
that fast. The design is already safe - the frame lives in a one-slot `watch` cell, so
a slow browser misses frames rather than queueing them, and a stalled one is dropped
after three seconds - so this is about waste, not about risk.

**Card 170 made this the only page**, which cuts both ways. There is no second page a
phone can sit on cheaply any more, so a page left open is always a frame stream; but
the page is also the thing somebody actually watches, so the frames are less often
wasted than they were. Card 170 did take the easy half: a player renders at 5 fps when
its panel is away *and* no socket is open (`Screen::watchers`), so an idle studio with
nobody looking costs almost nothing. What is left is the case this card is for - a tab
that is open, visible, and being sent more frames than it draws.

Where it starts to matter:

- over a tailnet, or on a phone, 372 KB/s per tab is a lot for a 64x32 picture;
- a backgrounded tab still gets the full stream (`requestAnimationFrame` stops, so the
  frames are received and thrown away);
- `docker stats` on `workbench.local` will show this as the studio's idle cost once
  card 107 lands, and it should not be the biggest number there.

A second, smaller case from the same card: dragging a parameter slider is about 60
`POST /api/v1/set_param` a second, each of which broadcasts a full `StudioState` to
every other browser. Bounded (the broadcast is 32 deep and coalesces on lag), but the
same "send what is useful" question.

## Deliverables

- A per-socket preview rate, chosen by the browser rather than by the server: a query
  parameter (`/api/v1/ws?fps=`) or a small client message. Default to something
  sensible rather than "everything".
- The UI stops asking for frames when the tab is hidden (`visibilitychange`), and asks
  again when it comes back.
- Measured before/after: bytes per second per socket, at 30 and 60 fps, hidden and
  visible.

## Acceptance

A hidden tab costs ~0 KB/s; a visible one costs what it asked for; the engine and the
panel link are unaffected either way (the existing stalled-browser test still passes,
and its numbers do not move).

## Log
