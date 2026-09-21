---
id: 126
title: The brightness slider bounces after it is moved
type: build
hardware: no
depends: [187]
owner: worker (sonnet)
branch: card/126-brightness-slider-bounces
---

## Goal

The owner, 2026-09-21, on the deployed Studio: "the UI for brightness on the web is janky -
I move the slider and it bounces around. It does seem to work, but not ideally." Make the
brightness control on both screens (`/` and `/panel`) stay where it was put.

## Context

- `crates/studio/ui/common.js`, `bindBrightness` (card 187): the control is an index into
  `boot.brightness_stops`. Its `show(d)` runs on every state message and, when the input is
  not `busy`, writes the slider from `d.telemetry.brightness` when there is telemetry, else
  from `player.health.brightness_applied ?? player.brightness`.
- The likely cause, to be confirmed not assumed: the panel's telemetry is periodic, so for a
  second or so after a `change` the newest telemetry still holds the OLD brightness. The
  moment the control stops being `busy`, `show` writes the old value back (the slider jumps
  back), and when the next telemetry arrives it jumps forward again. A drag that fires
  several `change`s, or a raise/cap where `applied != asked`, makes it worse. State messages
  are paced at up to 20 a second (card 196), so every stale one is a chance to bounce.
- Other sliders on the page (`bindSlider`, the rate slider of card 183, speed of card 197)
  may already have a rule for "the person just set this; do not fight them" - read them
  first and use the house pattern if there is one.
- What the control should mean: after the person lets go, it shows what they chose (or what
  the panel said it applied, when that differs - the floor and the cap are real) and holds
  it until the panel's telemetry agrees or clearly disagrees for good (say a few seconds),
  and only then follows telemetry again. A change made elsewhere (a second browser, the
  CLI) must still show up within a few seconds. The re-assert loop in `fleet.rs` is not
  part of this and must not change.

## Deliverables

- The fix in `common.js` (both screens use the one binding; keep it one).
- A test that pins it. `crates/studio/tests/ui.rs` holds the UI to textual rules; if the
  hold logic can be a small pure function in `common.js`, exercise it with `node` when node
  is on the machine (skip cleanly when it is not - no Node dependency for the build), or
  pin it server-side if the honest fix turns out to be there (e.g. the state message
  carrying "applied at" so the page can tell stale telemetry from fresh).
- `crates/studio/README.md` if the control's behaviour is described there.

## Acceptance

Against `screeny-sim` and a local Studio: set a brightness from the page's API path the way
the page does and watch the state stream - no state message after the change makes the
control show the old value. The owner moves the slider on the deployed page and it stays.

## Log
