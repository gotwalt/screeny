---
id: 172
title: The page's rate control cannot show a rate a script set
type: build
hardware: no
depends: [170]
owner: worker-173
branch: card/studio-page-tells-the-truth
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

### The card against today's tree

Unchanged. `page::RATES` was still `[30.0, 60.0]`, `set_playback` still did
`RATES.contains(&req.fps).then_some(req.fps)`, and `index.html` still had the
two-stop `#fps` radio group. The soak still uses 10, 15 and 24.

### The decision

**Option 2, the one the card thought was probably right.** A slider over
`player::MIN_FPS..=MAX_FPS` (1..60) with detents, and `set_playback` clamps
instead of ignoring - so the page's route and `player/set {fps}` take exactly
the same rates and can no longer disagree about what a rate is.

Option 1 (keep two buttons, add a readout beside them) would have left a
control whose two stops cannot express where the panel is, and clicking either
one would still have *changed* the rate. Card 170's standard is that there is
no control whose effect on the panel is unclear; two buttons that are both
unlit fail it.

`the_api_round_trips`'s assertion changed, as the card said it would:

```diff
-assert_eq!(play["fps"], 30.0, "45 is not one of the offered rates, so the rate does not move");
+assert_eq!(play["fps"], 45.0, "any rate the player may be on is accepted");
```

The commit says why. The route's **shape** is untouched - `{paused, speed, fps}`
in, the state out - so nothing that drives it has to change; what changed is
that a rate it used to silently drop now lands.

### What I did

- `page.rs`: `RATES` deleted, with a comment where it was saying what replaced
  it and why. `crates/art/README.md`'s conformance row pointed at it (and at
  the wrong file); it points at `MIN_FPS`/`MAX_FPS` now.
- `api.rs`: `set_playback` passes the rate through; the player clamps.
- `index.html`: a `.slider` from 1 to 60, step 1, with a `<datalist>` of stops
  at 1, 10, 15, 24, 30 and 60.
- `main.js`: the fps control is bound by hand rather than with `bindSlider`,
  because `bindSlider`'s output reads the *input*, and a range input with
  whole stops rounds a rate that has not got one. The readout says the rate
  the player is really on (10.5 reads "10.5 fps"); only the thumb rounds, and
  never by more than half a frame.
- `player.rs`: a rate that is not finite is refused rather than clamped.
  `f64::clamp` hands a NaN straight back and `Duration::from_secs_f64(NaN)` in
  the render loop panics - previously unreachable, because `set_playback`
  dropped everything that was not 30 or 60, NaN included. Card 172 made it
  reachable, so it is guarded here.

### Evidence

- `tests/ui.rs::the_rate_control_spans_the_players_whole_range`: the HTML's
  `min`/`max` are `MIN_FPS`/`MAX_FPS`, checked against the constants rather
  than against a second copy of the numbers, and the radio group is gone.
- `tests/ui.rs::a_rate_set_through_the_api_is_what_the_page_reports`: the
  card's acceptance. `player/set {fps: 10}` (and 15, 24, 45) - then
  `GET /bootstrap`, which is what a browser reloading draws its control from -
  says the same rate. An infinite rate leaves it where it was.
- `tests/api.rs::the_api_round_trips`: 45 and 10 land; 9000 clamps to 60; 0
  clamps to 1.
