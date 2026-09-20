---
id: 161
title: Thirty frames a second, and nothing else
type: build
hardware: no
depends: [151]
owner: worker (Claude)
branch: card/161-thirty-fps
---

## Goal

The owner, 2026-09-20: "i think we can remove 60fps support, as the display really can't do
much with it. let's just make everything target 30 with no variability for now."

One rate, 30 fps, everywhere on the art and Studio side: rendered at 30, sent at 30,
previewed at 30, and no control that offers anything else.

## Context

- Today the Studio's player renders at a per-player `fps` (1..=60, **default 60**) and hands
  every frame to `screeny::Link`, whose cadence (30) drops every other one: the deployed
  Studio's own numbers say `frames_offered` is twice `frames_sent`, the other half counted
  as `frames_coalesced`. Half the rendering is thrown away, and a patch's motion is sampled
  at 60 and shown at 30. `screeny-art play|pipe` default to `--fps 60` for the same
  historical reason (the art system was built to a 60 fps instruction; the design rate has
  been 30 since the brief was rewritten - `CLAUDE.md`: "Target: 30 fps").
- What goes: `fps` as a setting of a player (`PlayerChange`, `POST /api/v1/set_playback`'s
  `fps`, `player/set`'s `fps`, `StoredPlayer::fps`), the "Frames per second" slider and its
  stops on the Picture screen (cards 172 and 183 built it; their mechanism - `drawStops`
  for any slider with a datalist - stays for Speed), the Rate readout if it then says
  nothing, `--fps` on the CLI. One constant, `FPS = 30.0`, in one place
  (`screeny_art` or the player), used by all of them. `IDLE_FPS` (the 5 fps a player drops
  to when no panel is connected and nobody is watching) stays: it is not a rate anybody
  sees.
- **Nothing that works today may break**: an old state file with `fps: 60` loads and the
  key is ignored (say it once in `repaired`, in the style of card 102's retired `levels`);
  a request that still sends `fps` is accepted and the field ignored (the firmware
  session's scripts send only `device`/`on`, but be kind); `speed` is untouched - it scales
  a patch's time, not the frame rate.
- **What stays, and why - say this to the owner in the Log**: the sender's cadence ladder
  in `crates/screeny` (spec 6.9: under sustained loss a sender steps its rate down and back
  up) is protocol behaviour, normative, shared with the `screeny` CLI and the conformance
  suite, and invisible unless the network is losing frames. "No variability" is read as
  "no rate for a person to choose", not "delete the sender's loss response". Do not touch
  `crates/screeny`'s cadence, `crates/proto`, the spec, or `screeny stream --fps` (the
  bench tool measures the device at other rates on purpose).
- The browser preview asks the socket for `fps=30` already (card 120's `Pace`); the
  socket's `fps` query stays - it is how a hidden tab asks for 0.
- Patches that step their own state per frame rather than by `ctx.dt` would change speed
  when the render rate halves. Check every patch (the clocks' dances, metaballs, plasma,
  overland, the shader patches' time uniform) and show that motion is the same speed at 30
  as it was at 60 - by a test on `dt`-dependence or by reading the code, stated per patch.

## Deliverables

- The constant and the removals above; state/API compatibility with tests; the page
  without the rate slider; READMEs (`crates/art`, `crates/studio`) and
  `docs/design/generative-art-brief.md` where they say 60.
- In the Log: render cost before and after (the player's CPU on the dev machine, or
  frames rendered per second from `/api/v1/status`), since halving it is the side benefit.

## Acceptance

`/api/v1/status` on a running Studio shows `frames_offered` about equal to `frames_sent`
and nothing coalesced in steady state; no control on the page mentions a frame rate; an
old state file loads.

## Log

### Claimed

Branch `card/161-thirty-fps`, cut from `main` at `3b84a2d`. Read the card, `docs/README.md`,
cards 150/151 (the migration pattern and the `repaired` voice), 172/183 (the slider being
removed and `drawStops`, which stays), 198 (the two screens) and 164 (the link's stats in
`player.rs`, which this card must not disturb).

Survey before touching anything - where a rate is named today:

- `crates/art`: `snapshot::FPS = 30.0` (already the one 30 the snapshot tool steps at);
  `bin/screeny-art.rs` `--fps`, default 60, clamped 1..60, used by both `play` and `pipe`.
- `crates/studio`: `player::{MIN_FPS 1.0, MAX_FPS 60.0, IDLE_FPS 5.0}`, `StoredPlayer::fps`
  (default 60), `PlayerChange::fps`, `SetPlayback::fps`, `SetPlayer::fps`, `StudioState::fps`,
  `PlayerStatus::fps`, `PreviewStatus::fps`, `ws::{DEFAULT_FPS, MAX_FPS}`, the
  `#fps` slider and `#fps-stops` datalist in `index.html`, `showFps`/`pushPlayback` and the
  limiter tick's `state.fps` in `picture.js`.
- `crates/screeny`'s cadence ladder, `crates/proto`, `crates/sim`, `firmware/`,
  `screeny stream --fps`: **not touched**, by the card's instruction.
