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

### Before: what 60 costs, measured

Release build, loopback only: one `screeny-sim --headless --no-mdns --no-http --frame-port
49474 --control-port 49475`, one `screeny-studio --listen 127.0.0.1:8781 --no-discover
--no-device-http --state-dir <scratch>`, one panel added with `devices/add {play:true}`, the
default patch (`clocks-numerals`), no browser attached, both under `timeout 180`. Settled for
11 s, then a 30 s window off two reads of `/api/v1/status`:

| | per second |
| --- | --- |
| frames rendered (`player.health.ticks`) | **59.62** |
| `frames_offered` | 59.62 |
| `frames_sent` | 29.99 |
| `frames_coalesced` | **29.63** |
| `frames_dropped` | 0 |
| the sim's `frames_rx` | 29.96 |

`fps_measured` 62.8, the sim's interarrival 33.5 ms. **Half of everything rendered is thrown
away by the link's cadence ceiling** - exactly what the card says.

Process CPU over the same window, `ps -o cputime=` on the studio's own pid (not the `timeout`
wrapper): 3.83 core-seconds in 30 s = **12.8% of one core** on this M4.

### The Studio: what came out

Everything that let anybody name a rate, gone; everything that *reports* one, kept.

Removed:
- `player::MIN_FPS` and `player::MAX_FPS`. `IDLE_FPS` (5) stays, with a line saying why.
- `PlayerChange::fps` and the clamping branch in `Player::configure`.
- `StoredPlayer::fps` (and its 60.0 default), and `LegacyPreview::fps` with it.
- `SetPlayback::fps` and `SetPlayer::fps`. Neither struct declares the field now and
  neither has `deny_unknown_fields`, so **a body that still sends `fps` is accepted in
  full and the field does nothing** - `tests/ui.rs::a_rate_sent_by_an_older_client_is_
  accepted_and_ignored` posts 10, 15, 24, 45, 60, 0, `1e400` and `null` to `player/set`
  and card 172's `{paused, speed, fps}` to `set_playback`, and checks the rest of each
  body was applied.
- The `#fps` slider, its `#fps-stops` datalist and `showFps` from the Picture screen;
  `pushPlayback` no longer sends a rate. `drawStops` stays, as card 183 built it: it is
  Speed's mechanism now, and `ui.rs`'s stop test belongs to Speed.

Kept, and why:
- **The Rate readout (`#ro-fps`) stays.** It never was the setting: it is
  `core.fps`, the rate the render loop is actually achieving, off the frame packet. It
  is not a control, and it is the one place a machine that cannot hold 30 says so - a
  debug build will not (`crates/studio/README.md` line 16). A readout of reality is
  worth keeping when there is one rate; a *control* is not.
- `StudioState::fps`, `PlayerStatus::fps`, `PreviewStatus::fps`: reported, never set,
  always `screeny_art::FPS`. This is what lets the page draw the limiter's per-frame
  tick without writing 30 into the JavaScript - there is still exactly one constant -
  and it keeps every script that reads `/api/v1/bootstrap` or `/status` working.
- `ws`'s `fps` query: `DEFAULT_FPS` and the pace ceiling now both read
  `screeny_art::FPS`. `0` still means "send me nothing", which is how a hidden tab gives
  up its claim, and asking for more than is rendered simply means "everything".

**No schema bump**, and the card guessed right: nothing about the file's *shape*
changed, only that one key stopped meaning anything - exactly card 102's `levels`. A
bump would send every deployed studio through a migration, a backup copy and a
`recovered` sentence to delete one number. `note_retired_fps` says it once, in the
`repaired` voice, listing the values it found; the key is dropped for good on the next
save. Tests: `a_retired_fps_loads_and_is_reported` (60, 10 and 30, each checking the rest
of the file survives, that there is no backup file and no recovery) and
`a_retired_fps_is_said_once_for_the_whole_file` (two players, one sentence).

Docs: `crates/studio/README.md` (idle rate, the two route tables, the socket's default,
the test index), `crates/art/README.md`, `docs/design/generative-art-brief.md` (the
frame-rate row, and rule 5 - which said "keep your 60 fps loop, do *not* solve this by
rendering at 30" and is now reversed, saying so, with "step by `ctx.dt`" in its place),
`docs/design/studio-vision.md`.

`cargo clippy --workspace --all-targets`: silent.

### Every patch, time or frames

**Nothing stepped per frame. No patch needed fixing**, so no patch's look changed by this
card at all.

`crates/art/tests/rate.rs` is the evidence, and it covers `patches::ALL` rather than a
list written here, so a patch added later (flock, card 168) is covered the day it lands.
Per patch it renders to the same engine time `T = 6 s` twice - 60 steps a second and 30 -
and compares the last frame with a third run that takes the **same number of steps as the
30 fps one but only reaches `T/2`**, which is exactly what a frame-counting patch would
draw at 30. The clock is pinned so the time-telling patches see the same seconds in every
run, and is placed so the minute rolls over at `0.75 T`: between the half-time run's end
and the full run's, or the clock patches would sit still through both and the comparison
would prove nothing (asserted, per patch, with `half_time > 0.002`).

Measured, mean absolute difference per channel in linear light:

| patch | 60 vs 30, same `T` | 60 vs half the time | reading the code |
| --- | --- | --- | --- |
| clocks-numerals | 0.00023 | 0.06900 | `ctx.t` for the plan (`began`, `tau = ctx.t - began`) and `ctx.dt` for the ambient field and `settle()`. The servo integrates `velocity * dt` and clamps acceleration by `motor.acc * dt`; the mood eases by `1 - exp(-dt/7)`. Time, throughout. The residual is Euler integration landing a hair apart, not a speed difference. |
| clocks-dials | 0.00004 | 0.06149 | the same field, through `step_holding(..., ctx.dt, ...)`. |
| vesta | 0.00000 | 0.00369 | pure `ctx.t`: `born = ctx.now - ctx.t`, each module's `began: ctx.t`, `pose(ctx.t, fall)`. The only rate-dependence left is *which frame* notices a minute has rolled over, which is at most one frame either way and is inherent. |
| plasma | 0.00000 | 0.37438 | `let t = ctx.t as f32` and nothing else; no state. |
| metaballs | 0.00000 | 0.10576 | `ctx.t * ctx.get("speed")`; no state. |
| overland | 0.00000 | 0.09341 | `ShaderPatch`: `u.t = ctx.t`. `u.dt` is uploaded but **no `.wgsl` in the crate reads it**; the day cycle is `ctx.t` in `scene()`. |
| lattice | 0.00000 | 0.07340 | the same `ShaderPatch`, all of it in `lattice.wgsl` off `u.t`. |
| knot | 0.00000 | 0.08723 | `spin = ctx.t * speed`; the rest is `u.t`. |
| testcard | 0.00000 | 0.00334 | `line_x = (ctx.t * speed).rem_euclid(W)`; no state. (Its half-time figure is small because one moving column sits on a static card - still six times the threshold.) |

The six with no state at all are asserted **exactly equal** at the two rates, not merely
close (`PURELY_A_FUNCTION_OF_T` in the test): a patch that starts accumulating state
shows up there first. The three stateful ones are asserted to be at least four times
closer to the same-`T` run than to the half-time one; they measure 300x, 1500x and
exactly equal.

The pipeline and the limiter are time-based too, and were already: `Pipeline::process`
takes the wall `dt`, and `Limiter::apply` eases by `1 - exp(-dt/0.5)` and rises by
`max_rise_per_s * dt`. So the limiter behaves the same at 30 as at 60.

Also added: `there_is_one_rate`, which pins `screeny_art::FPS` at 30 and checks
`snapshot::FPS` is the same constant rather than a second copy of it.
