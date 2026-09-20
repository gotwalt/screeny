---
id: 177
title: flock - less predictable turns, and real vertical motion
type: build
hardware: no
depends: [168]
owner:
branch:
---

## Goal

The owner, 2026-09-20, watching `flock` on the panel ("this is great"), then: "let's also
randomize the bird direction changes a bit more - i'd love to see some more vertical motion
change." (His other ask in the same message - a toggle for the whole backdrop, white birds
on entirely black - is done: the `backdrop` toggle, by the orchestrator on `main`.)

The flock should surprise more: direction changes that are less regular in when and how
hard they come, and climbs, dives and swoops that are part of the flight rather than a
wobble about one cruising height.

## Context

- Card 168's Log and the module header of `crates/art/src/patches/flock/sim.rs` say how
  the flight works today: boid rules with `calm` setting speed band and turn rate; nothing
  climbs or dives past **25 degrees**; a floor/ceiling with a 13 m margin **and a standing
  spring to the cruise band**; five drifting blobs; a slow attractor re-picked every
  40-110 s. The spring and the pitch limit are why the flight is mostly horizontal; the
  long attractor period and the slow blob drift are why turns feel regular.
- Ideas, not orders - try them and keep what reads: a taller world with the spring much
  weaker or replaced by soft floor/ceiling only; the attractor picked in 3D with real
  height differences, on a shorter and more varied clock; occasional **events** drawn from
  the RNG - a sudden shared dive and recovery (what a murmuration does when a hawk comes),
  a rising spiral, a split and rejoin - with long quiet flight between them so they stay
  events; per-bird restlessness (small random steering) so the flock's shape breathes; a
  pitch limit that depends on speed (a dive is faster, a climb slower) so vertical motion
  looks like flying and not like an elevator. Wingbeat already glides on descent: make
  climbs beat harder.
- **The camera is the constraint.** It is a bird, and everything the flock does it has to
  follow without the view becoming nauseous: card 168 measured view yaw p95 15-19 deg/s
  (hard cap 26) and roll p95 ~2.5 deg/s, 34-48 of 55 birds in frame. Pitching the view is
  new: with the backdrop on, the horizon will now move up and down the panel, which is
  exactly what will make a dive read - but keep it smooth, and keep the flock in frame
  through a dive (the camera may need to anticipate: follow the flock's mean vertical
  velocity, not just its position). Re-run card 168's long-run measurements on seeds 11,
  29 and 404 and report them beside the old numbers, plus new ones: the flock's altitude
  range and vertical speed distribution over ten minutes, view pitch rate p95/max, and the
  distribution of time between noticeable heading changes (it should be broad).
- With the backdrop off (white on black) there is no horizon, so vertical motion has to
  read from the birds alone: their pitch attitude, glide vs beat, and their drift across
  the frame. Look at strips in both modes.
- The owner runs it slowly (`pace` 0.7 default, he may go lower): an event must still be
  graceful at `pace` 0.3.
- Give him one or two controls, not five: e.g. `wild` (how often and how hard the flight
  changes its mind) and `lift` (how much of the motion is vertical). Defaults noticeably
  livelier than today's, but still calm.
- Determinism, rate independence (fixed 1/60 s step), the exact-on-the-wire palette, and
  every test of card 168 must still hold. `seeded: true`.
- Noted while adding the backdrop toggle: on black the birds have only `INK = 6` levels
  for both anti-aliasing and depth. If depth steps are visible as birds come and go,
  consider more ink levels when the backdrop is off (the sky bands are unused then, so the
  palette has room) - measure bytes and exactness if you do.

## Deliverables

The flight changes and the controls; tests extended; strips in the scratchpad's `flock/`
(a dive and recovery with and without the backdrop; a minute's centroid path, old against
new); the measurements above in the Log; "Open with the owner".

## Acceptance

The owner's eye on the panel. Before that: strips in which a dive reads as a dive, and
numbers showing the view stayed smooth and the flock stayed in frame.

## Log
