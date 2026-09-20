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

### `Gate`: the state stream paced the way the frames are

`crates/studio/src/ws.rs` grew a `Gate` beside `Pace`, one per socket, with one
constant: `STATE_GAP = 50 ms`, which is 20 messages a second. Three edges, as
the card asked:

- **leading**: a change more than a gap after this socket's last state message
  goes out where it stands. A piece picked or a switch flipped is as immediate
  as it ever was; only a burst is thinned.
- **coalescing**: inside the gap, the newest state (by `rev`) *replaces*
  whatever was held. One slot, newest wins - the same shape as the frame cell
  and the state file's mailbox.
- **trailing**: the held one goes out when the gap is up. This is the one place
  state differs from frames: a frame that is not due is **dropped**, a state
  change is **held**, because the value a drag ended on is what the other
  browser is left resting on.

The trailing edge is a fourth arm of the socket's `select!` whose future is
`sleep_until(last_at + STATE_GAP)` when something is held and `pending()` when
nothing is, so a socket with no changes to deliver schedules nothing at all.

One subtlety worth writing down. A socket is not told about its **own**
changes, and it never was - but now that a message can be held, a browser's own
change can arrive while somebody else's is waiting, and delivering that held
message 50 ms later would carry a whole state that predates this browser's own
change and would put its own control back where it was. So `skip_own` keeps the
held message but refreshes its `state` and `rev` with the newer one: whose
change it was is not echoed, the newest state still is. Before this card the
same thing was true by luck of ordering; now it is on purpose.

**After**, same test, same bench:

| | before | after |
|---|---|---|
| state messages to the second browser | 60.0/s | **19.3-19.7/s** |
| bytes | 24 949 B/s | **8 038-8 178 B/s** |
| a single deliberate change | immediate | **4.3 ms average, 21 ms worst of ten** |
| the value a drag ended on | arrives | **arrives, 25-51 ms after the drag stopped** |

So a drag costs the second browser a third of what it did, the page's own
changes still arrive at once, and nothing is left resting on a stale value.

The page's JS was **not touched**: `ui/main.js` ignores `rev` and `from`
entirely and calls `sync(message.state)`, so a paced stream of newest-wins
states is exactly what it already wanted.

Tests, all new, in `crates/studio/tests/pacing.rs`:
`a_drag_costs_the_second_browser_a_bounded_number_of_messages` (the measurement
above, now with bounds), `a_single_change_still_arrives_at_once`,
`the_value_a_drag_ended_on_always_arrives` (three drags, each ending somewhere
else). `tests/api.rs::two_browsers_see_each_others_changes` was not edited and
still passes as written.
