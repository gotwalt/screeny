---
id: 142
title: The dashboard says what each panel plays, but does not show it
type: build
hardware: no
depends: [106, 120]
owner:
branch:
---

## Goal

"What is each panel playing" is answered on the dashboard with a piece name, a seed and
some counters. The obvious answer is a picture: a small live thumbnail per panel, so a
glance really is a glance.

## Context

Card 106 deliberately left device players out of the preview socket. Every open socket
already gets every frame of the *design view* - 6196 B at 60 fps, 372 KB/s a tab - and
card 120 is the card for making that sane. Adding one stream per panel on top of that,
to a page that is meant to be left open on a phone, would be the wrong order of work.

So: 120 first, then this, on 120's terms - a thumbnail is a few frames a second, not
sixty, and only for panels whose card is actually on screen.

## Deliverables

- A per-player frame cell and a way for a browser to subscribe to one at a low rate.
- Thumbnails on the dashboard cards, subscribed only while visible
  (`IntersectionObserver`, no library) and dropped when the tab is hidden.
- Measured: bytes per second with one panel and with four, tab visible and hidden.

## Acceptance

Four panels on the dashboard on a phone cost less than one design view tab does today.

## Log
