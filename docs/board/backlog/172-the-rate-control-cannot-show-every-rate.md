---
id: 172
title: The page's rate control cannot show a rate a script set
type: build
hardware: no
depends: [170]
owner:
branch:
---

## Goal

A player may render at any rate from 1 to 60 fps (`player::MIN_FPS`/`MAX_FPS`), and
`POST /api/v1/player/set {fps}` takes any of them - the soak uses 10, 15 and 24. The
page offers **30** and **60** and nothing else, so a panel a script set to 10 fps shows
a rate control with *neither* button lit, and clicking either silently changes the
rate rather than revealing it.

## Context

- `crates/studio/ui/index.html`, the `#fps` segmented control; `crates/studio/src/api.rs`,
  `set_playback`, which deliberately ignores a rate that is not in `page::RATES`
  (card 105's behaviour, kept exactly by card 170 so `the_api_round_trips` still holds).
- Two honest answers, and the second is probably right:
  1. keep the two stops and add a readout beside them for "actually 10 fps";
  2. make it a slider over 1..60 with detents at 30 and 60, and let `set_playback`
     clamp rather than ignore - which means changing `the_api_round_trips`'s
     "45 is not one of the offered rates" assertion and saying why in the commit.
- Related but not the same: **card 163** (parameters that are named stops should be
  drawn as a list rather than a slider). Whoever does one should look at the other,
  because both are about a control whose shape does not match its values.

## Deliverables

- A rate control that can show any rate a player may be on.
- `set_playback` and `player/set {fps}` agree about what is accepted, or the page
  explains the difference.
- A test that a rate set through `player/set` is what the page reports.

## Acceptance

`curl player/set {fps: 10}`, reload the page: it says 10, and does not change it.

## Log
