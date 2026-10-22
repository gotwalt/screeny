# 011: Hearth at a real speed, burning wood

Status of the `exp/hearth-speed` branch, written 2026-10-22 as the work moves
from the owner's Mac to workbench and another session picks it up. Merged
to `main` the same night, at his word, and deployed to both panels: the
grid follows `speed`, small steps by default, the slider tops out at 18, and
the frame is held steady (below).

## What the owner asked for

Looking at hearth on the panel (2026-10-21): "amazing, but also a bit in slow
motion". The picture he wants: a fire running *inside* the Tidbyt, the LEDs
the inside of a firebox, the chimney at the top. A control for real versus
slow motion, but he wants to see real speed. Then: "the goal isn't more FPS,
but rather the right speed"; and "realistic if this were a gas grill, but the
idea is to simulate wood burning just off screen" - "wood burning does not
have a fuel injection velocity".

On 2026-10-22, on the 8x-11x speed with the `wood` and `expansion` sliders:
"pretty good directionally". He also saw the fire reach off the right side of
the panel but not the left, and asked for the fuel source to be symmetric.

## Measured facts

- The panel's LED area is 7" wide (64 LEDs at 2.78 mm), so 32 rows are about
  3.5". He measured the screen 4 11/16" tall, so there is about 15 mm of black
  margin above and below the LEDs (inferred, not measured directly). The
  branch puts the woodpile in the bottom margin (`BELOW` = 16 design units)
  and lets the ceiling rise into the top one (`headroom`).
- A design unit is about 1 mm at the hearth's middle plane, 3 to an LED.
- Card 376's gas rises at a median ~35 mm/s (p90 ~55) in frame. Real flame gas
  is ~1-1.5 m/s (McCaffrey), puffing at 5-7 Hz. So the shipped fire is about
  30x slow motion; by Froude scaling it moves like a fire ~1000x bigger, which
  is why it reads as grand.
- One fluid step is ~2.6 ms on an M4 (advect 1.2, pressure 0.7, vorticity
  0.3; `hearth::tests::stages --ignored`). At small steps that caps the Mac at
  about 8-11x speed at 30 fps. **Workbench has an Intel iGPU, not the M4: the
  cost there is unmeasured. Run `hearth::tests::cost` and `stages` first.**

## Findings so far

1. Speeding up with big steps (several steps' worth of dt per GPU substep)
   gives smooth, pointed *jets*: numerical diffusion. Small steps (env
   `HEARTH_BIG=1`) give ragged, broken flames, which look right. Real speed
   (~30x) at small steps fits no GPU we have. Not yet tried: medium steps
   (`HEARTH_BIG=2`), fewer pressure sweeps per substep, MacCormack only on the
   last substep.
2. Even at 8x with small steps he called it slow and a gas grill. Cause: the
   bed was a burner by construction. Fuel was held at a level in a flat sheet,
   the tongue noise drifted in wall time (standing still under fast gas), and
   the burn's expansion at the fixed bed acted as an injection nozzle.
   The branch's `speed` puts the tongues on gas time; `expansion` turns the
   nozzle down; `wood` breaks the release into pockets.
3. **The left-right lean** came from the log layout. Each log was jittered
   ±10 units with no re-centring, and fuel fell off as a Gaussian, so it ran
   past the nominal ends. Seed 1's bed ran 30..172 of 192: centred 5 units
   right, with the right log's tail reaching the edge. Now the pile is centred
   and its reach capped at `SPREAD` of the width each side, for every seed.

## What the branch has

Commits on `94bf734`, in order:

- `speed`: gas time multiplier (logs, breathing, gusts keep wall time).
  `headroom`: the ceiling above the top row, in LEDs. `HEARTH_BIG` env: the
  most fixed steps one GPU substep may cover (default 4 gives jets; use 1).
- `wood` (pocketed release) and `expansion` (share of the burn expansion;
  ~0.1 for wood).
- **The woodpile** (this commit). The logs are solid capped cylinders: three
  or four on the floor, never parallel, plus one or two laid across a gap
  resting on both. The gas flows round them: they are solid in the pressure
  solve. Fuel comes off their surfaces in a shell (`SHELL` = 1.6 units): tops
  most, undersides least, cut ends hard, and hardest in the gaps where two
  surfaces nearly touch. The fuel level thins toward the shell's edge;
  without that it was a solid wall of flame filling the frame. Sparks leave
  from the logs' tops.
  - The geometry is worked out once (`bed_cells`). Each step refreshes only
    the cells that give off gas or glow, and uploads the bed once per step,
    not once per substep.
  - The bed is now a 3D block of the bottom `bed_rows` rows (vec4: fuel,
    glow, inside-a-log flag). `solid()` in the shader reads it, so every pass
    that tests solidity binds it.

At the owner's last settings (speed 11, wood 0.94, expansion 0.1, size 1.08,
draft 0.88, embers 1.61, flue 0.26, gusts 1.33, headroom 3.6, smoke 0.16,
seed 1), the woodpile's fire is about card 376's size, with more separate
tongues. **He has not seen the woodpile on the panel yet**; that is the next
step.

## How to show it to him

He judges on the panel, live, sliders in hand. On the Mac this worked:

- Run the branch's Studio on a spare port with its own state dir:
  `HEARTH_BIG=1 screeny-studio --listen <addr>:8788 --state-dir <dir>
  --no-discover --no-device-http`.
- Borrow one panel from the deployed Studio: `POST /api/v1/set_panel
  {"on":false,"panel":<device>}` there, then `{"on":true,"to":"screeny-<device>"}`
  on the branch Studio. Find the device ids with `GET /api/v1/panels`.
- Give the panel back when done.

Keep it running until he says to stop; don't put a time limit on it. No
warm-up: the fire lights on screen.

## Renderer performance on workbench (2026-10-22)

Workbench has an Intel UHD (Raptor Lake-P, Vulkan); the deployed Studio
renders on it too. A fluid step costs about 19 ms of GPU time there, against
2.6 ms on the M4. At the owner's settings (speed 11), small steps
(`HEARTH_BIG=1`) take ~210-270 ms of GPU a frame (~4 fps); `BIG=4` takes
~48 (~15 fps, and jets). Running speed 11 at 30 fps with small steps needs a
step in ~3 ms: about 6x. The owner's call: improve the renderer.

How to measure here:

- Measure at speed 11, not 1. At light load the iGPU clocks down (100-1400
  MHz), so `stages` at speed 1 reads ~21 ms a frame whatever the work, and
  time saved in one stage shows up in another. At speed 11 it holds 1400
  MHz. `/sys/class/drm/card0/gt_act_freq_mhz` shows the clock.
- The deployed Studio shares the GPU: expect ±25% between runs.
- Run `stages` and `cost` one at a time. Together (two devices in one
  process) they ran past 20 minutes; `cost` alone takes 20 s (33 ms a frame
  at speed 1).
- `survey` now takes `HEARTH_SET`. With `BIG=1` a step is the same at any
  speed, so the picture can be compared at speed 1 with the other settings.

GPU ms a frame at speed 11, `BIG=1`, by `HEARTH_GRID` depth and sweeps:

| depth, sweeps | advect | pressure | total |
|---|---|---|---|
| 48, 16 (card 374) | 94 | 126 | 271 |
| 48, 8 | 98 | 65 | 212 |
| 36, 16 | 77 | 88 | 197 |
| 36, 8 | 76 | 46 | 163 |
| 24, 8 | 54 | 32 | 106 |

- **Sweeps: now 8.** The pressure carries from step to step, so 8 sweeps
  survey and look as 16 do (lit 33/30/34% against 34/29/32% for seeds 1-3
  at his settings). 4 flattens the fire (lit ~20%), 2 more so. Every hearth
  test passes.
- **Depth: not changed.** `depth` is the firebox's depth, not the cell size,
  so a shallower box changes the fire: at 36 the default fire is taller and
  fails `the_top_is_mostly_black_at_the_default` (3.0% of the top third lit
  at 16 sweeps, 5.4% at 8; the limit is 3%). 36 is the least that holds every
  woodpile (logs reach ±18 in z); 24 pins the logs into the walls. It needs
  his eye on the panel and a retune of `size`.
- The workgroup shape (4x4x4 now) is worth ~13% at 8x8x1, all of it in
  pressure. Not taken yet.

**Resolution is the big lever.** GPU ms a frame at speed 11, `BIG=1`, 8
sweeps, by cells per design unit (`k`):

| k | grid | total |
|---|---|---|
| 0.667 (card 374) | 128x88x32 | 211 |
| 0.5 | 96x~66x24 | 67 |
| 0.4 | ~77x~53x20 | 38 |
| 0.333 | 64x44x16 | 19 |

Card 374 chose 0.667 because 0.5 rounded the tongues, but that was judged in
slow motion. In stills at speed 11, 0.5 and 0.4 look like 0.667; 0.333 is
still tongued but taller and fuller (reaches the top row), so `size` wants
a retune there. Env `HEARTH_K` sets `k` in the player (not in tests).

On the Living Room panel (2026-10-22), k 0.333, speed 11, `BIG=1`, his
settings above, 30 fps: "that looks pretty good. big step in the right
direction". The deployed Studio was rendering the Office panel on the same
GPU meanwhile.

**The grid follows `speed`** (`GRIDS` in `mod.rs`): 2/3 up to 1.5, 1/2 up
to 4.5, 0.4 up to 8, 1/3 up to 12, 1/4 above. Each switch carries the fire
over (the fields resampled on the CPU, the sparks rescaled), and the old
grid's sim is kept so going back compiles nothing.
`the_fire_carries_to_another_grid` checks the first frame after each switch
is still burning. `HEARTH_K` pins the grid instead.

The owner, same evening, on the panel with the deployed Studio stopped (so
the GPU was the branch's alone):

- At 4 (1/2): "the detail is noticeable at this pace".
- At 18 (then on 1/3, ~90% GPU busy at 1400 MHz): "18 is probably the max
  of gas move speed. it's realistic enough". The `speed` slider now tops
  out at 18 (was 32).
- At 18 on 1/4 (~57% busy, the clock down to ~900 MHz), chunkier in
  stills: "the frame rate makes up for it".

**Small steps by default.** `BIG` is now 1 (it was 4, the jets); the branch
Studio had run with `HEARTH_BIG=1` all along.

**The bench rule at speed.** `no_whole_frame_pulse_above_three_hertz` now
runs at speeds 1, 4, 8, 11 and 18. Before a limiter the fast part of the
whole frame's level was 1.5, 6.4, 13.5, 24 and 35% of its mean (the limit is
15%; the brief: "no full-field flashing above 3 Hz ... build this in as a
limiter"). `paint` now holds each painted frame within 5% of its own
low-passed level (0.3 s), finding the gain by halving, since the ramp makes
the level rise ~3x faster than the light: 1.3, 5.1, 6.4, 7.7 and 9.4%. A
two-step secant search in its place jumped 33% in one frame while the fire
was catching.

A rough GPU monitor for this box without root (`intel_gpu_top` needs perf
access): whole-GPU busy from `gt/gt0/rc6_residency_ms`, one process's share
from `drm-engine-render` in its `/proc/<pid>/fdinfo`.

The Living Room panel advertises itself as "Living Room", so a Studio
reaches it with `{"on":true,"to":"Living Room"}`, not `screeny-4a00a4`.

Where the rest is (planned, in order of expected win):

1. **Advection** (~45% of a step after the sweeps cut): the trilinear taps
   are 8 buffer reads each, MacCormack takes ~13 of them a cell, and the
   same backtrace is traced three times (`adv_fwd`, both `adv_fix_*`).
   Trace once and keep it; then sample half-float 3D textures with the
   hardware filter (sampled textures are inside the downlevel limits; copy
   the buffers in once a step).
2. **Pressure**: a small multigrid instead of more sweeps, if 8 still costs
   too much.
3. **Per step on the CPU**: one submit a frame rather than one a substep.

## Next

1. Motion blur for fast embers: the smear uses only the last substep.
2. The default `speed` is still 1 (the slow fire, "beautiful"); the
   panels play whatever their Studio has saved.
3. The grid levels are workbench's. A faster GPU (the M4) could hold finer
   grids at each speed; they could follow measured frame time instead.
4. Embers do not collide with logs yet; they leave from above them.
5. `flock::tests::the_shipped_defaults_still_flock` fails on workbench
   ("a bird turned at x1.028 of its limit"); this branch does not touch
   flock, so it is not the hearth. Not looked into.
