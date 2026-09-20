---
id: 120
title: Preview bandwidth - pace the WebSocket to what a browser can use
type: build
hardware: no
depends: [105]
owner: worker-120
branch: card/120-183-182-studio-small
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

### The socket carries a pace, and the browser sets it (worker-120)

**What was added.** A preview socket now has a *pace*: how many frame packets a
second it wants, and whether it wants a picture identical to the one it was last
sent. Set on the way in (`/api/v1/ws?fps=10&repeat=false`) and changed at any
time with one new client message:

```json
{"type":"preview","fps":30,"repeat":false}
```

`fps: 0` means no frames at all - what the page sends when its tab is hidden -
while the state changes and the twice-a-second heartbeat carry on, so a hidden
tab stays correct for about half a kilobyte a second and is right the moment it
is looked at again. Both default to what a socket got before this card
(`ws::DEFAULT_FPS` = 30, `repeat` on); anything else a browser says, including a
message from a newer page, is ignored rather than fatal.

Nothing re-renders or re-encodes for a browser: the only question a browser
answers is *which* of the panel's frames it is sent. **Pacing is done by
dropping** a frame where it stands - never by holding one - which is the same
thing that already happened to a browser too slow to read one, so it cannot
slow the render loop or the panel link down.

One subtlety worth writing down: the frame **nearest** the deadline goes, not
the first one past it. `since + step/2 >= gap`, where `step` is however long it
has been since the previous frame arrived. Without it a 30 fps cap on a 60 fps
player whose frames land a fraction early takes every *third* frame and delivers
20; the first measured run showed exactly that (23.5 fps for a 30 fps cap).

**`watchers()` no longer means "a socket is open".** It means "a socket is being
sent frames", counted by `page::Viewer` (a guard, so a browser that vanishes
gives its claim back however it goes). This is the half of the card that is not
about bytes: a phone left on the page in a pocket must not hold a core at 60 fps
for a month. A studio with no panel and only hidden tabs open now idles at
`player::IDLE_FPS` exactly as if nobody were connected, and picks up again
within one idle frame - measured at 54 fps watched -> 5 fps hidden -> 60 fps
watched again, in
`preview.rs::a_hidden_tab_does_not_hold_the_player_at_full_rate`.

**Bytes per second per tab**, measured over 30 s windows on loopback with the
player at 60 fps ("before" is a socket asking for every frame with repeats,
which is byte-for-byte what every socket got before this card; "the page" is
what `ui/main.js` now asks for when visible; "slow link" is what it asks for
when `navigator.connection` reports save-data or a 2g/3g link):

| piece | before | the page (30, no repeats) | slow link (10) | hidden tab |
|---|---|---|---|---|
| `clocks-numerals` | 344.4 KB/s (56 fps) | **129.6 KB/s** (21 fps) | 45.1 KB/s (7.2 fps) | **0.52 KB/s** (0 fps) |
| `overland` | 369.2 KB/s (60 fps) | **187.6 KB/s** (30 fps) | 62.3 KB/s (10.0 fps) | **0.10 KB/s** (0 fps) |
| `plasma` | 346.3 KB/s (56 fps) | 192.4 KB/s (31 fps) | 62.3 KB/s (10.0 fps) | 0.10 KB/s (0 fps) |

So: a visible tab is roughly halved, a clock face is cut to a third, a phone on
a slow link to a sixth, and a hidden tab to a five-hundredth - the heartbeat and
nothing else. (`clocks-numerals`'s hidden cost is five times the others' because
its heartbeat carries a composing piece's "now playing" detail.)

**How still is a held clock, really?** Worth knowing, because the card's guess
was "identical for 15 s at a time". Measured over 20 s at 60 fps: `clocks-
numerals` with its defaults is identical to the previous frame **605/1143** of
the time, `clocks-dials` **0/1161** (it never stops moving), and numerals with
its resting dials switched to "as it was" only 64/1135 - the resting treatment
is most of the stillness. The 10 s window I measured first happened to land in a
moving stretch and showed no saving at all, which is why the table above is over
30 s: a minute of this piece is roughly half held and half moving.

**The cost of the check** is one 6 KB `memcmp` per frame per socket, and only
when `repeat` is off. No compression dependency was added, as the card asked.

**Status.** `/api/v1/status` grew a `sockets` object - `open`, `watching`,
`frames_sent`, `bytes_sent` - so "what are the browsers costing" is answerable
without `docker stats`. Additive; nothing else on that route moved.

### In a real browser

A Studio of its own: `--listen 127.0.0.1:8813 --no-discover --no-device-http
--ui-dir crates/studio/ui`, a temp state dir, and nothing near the bench panel
or `workbench.local`. Stopped afterwards; `ps` clean.

**The extension drives a window that is genuinely in the background**, so
`document.hidden` was `true` the moment the page loaded - which is the best
possible first result: `open: 1, watching: 0, frames_sent: 0` and 4.6 KB of
JSON, with no picture asked for at all. To drive the *visible* path from there I
took over `document.hidden` in the page and dispatched `visibilitychange`, which
exercises the page's own `previewFps()` and its message, and measured against
the server's counters with real elapsed time (`setTimeout` is throttled in a
background tab; the first measurement was wrong by exactly that factor before I
noticed).

| what | frames/s | bytes/s | `sockets` |
|---|---|---|---|
| one tab visible | 29.5 | 184 KB/s | `watching: 1` |
| that tab hidden | 0 | 1.27 KB/s (two sockets' heartbeats) | `watching: 0` |
| visible again | 28.9 | 180 KB/s | `watching: 1` |
| **two** tabs visible | 59.8 | 372 KB/s | `watching: 2` |
| back to one visible | 30.1 | 188 KB/s | `watching: 1` |

So the counter really does stop and start with the tab, the claim is given back,
and two tabs cost exactly twice one. A tab closed gives its socket back:
`open` fell as tabs closed, and a hidden page left connected keeps costing
530 B/s of heartbeat and nothing else, measured over six seconds with no tab in
the foreground.

**Not checked in a browser, and honestly so:** `requestAnimationFrame` does not
run at all in that background window, so the canvas stays black there and
`#ro-fps` reads `–`. What the page *draws* could not be judged by eye in this
environment. The frame pump was not touched by this card - the same bytes arrive
on the same handler - and `tests/panel.rs` still proves the browser's bytes and
the panel's are the same. The owner should glance at the page once after this
merges.

Console: clean, on load and after a reload.

