---
id: 121
title: Something that checks the studio's front end
type: test
hardware: no
depends: [105]
owner:
branch:
---

## Goal

`crates/studio/ui/main.js` is about 570 lines with nothing checking it. Everything
underneath it is tested; the layer the owner actually looks at is not.

## Context

Card 105 rewrote the front end's transport (`invoke()` -> `fetch()` + WebSocket, the
frame poll -> a push, two polling timers -> one heartbeat) and its state handling (a
second browser's changes are adopted without rebuilding controls). The server side of
all of that is tested from a hand-written client; the browser side was checked by:

- `node --check` for syntax;
- a cross-check that every `#id` `main.js` reaches for exists in `index.html`, and
  that every command it calls is a route the server has;
- the server's own tests, which pin every message shape the UI consumes.

None of that would catch a wrong argument to `renderer.upload`, a control that stops
refreshing, or a WebGL shader that fails to compile. A live check was not possible in
the worker's environment: the Claude browser extension was not connected.

The constraint that shapes this: **no Node toolchain and no JS build step** is a
deliberate rule for the product (`docs/design/studio-vision.md`, and the card 105
brief). A test-time dependency is a different thing from a build-time one, but it
should be argued for rather than assumed - and an answer that needs nothing at all
(a headless browser already on the machine, driven over the DevTools protocol from a
Rust test) may be better than one that needs npm.

## Deliverables

- Some automated check that the page loads, draws a frame, and survives a state change
  from a second client, against a real `screeny-studio` on an ephemeral port.
- Whatever it needs written down in `crates/studio/README.md`, including how to run it
  without the dependency if it has one.

## Acceptance

The check fails if `main.js` stops drawing frames, and it runs in the normal test
suite or is one documented command away from it.

## Log
