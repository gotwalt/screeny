---
id: 195
title: Studio device health - thresholds the firmware session measured, and "it may have crashed"
type: build
hardware: no
depends: [180, 192]
owner: worker-195
branch: card/195-device-health-thresholds
---

## Goal

The Device block (card 180) warns when the panel is actually in trouble, using numbers from
the people who measured the panel, and says honestly when a reboot may have been a crash.

## Context

Card 180 chose its thresholds from two readings. The firmware session, which measured the
real device across several firmware builds, replied (2026-09-20):

1. **Stack: `low_stack < 2048` is too late.** Interrupts land on core 0's stack at 256 bytes
   of context per level, and `stack_free` is a high-water mark that only ever falls. Healthy
   0.4.3 reads ~17-20 KB; the 0.4.0 build that worried them read 4-5 KB. **Warn below 8192,
   fault below 4096.**
2. **Heap: > 85% is a good fault line.** Steady state 45.6 KB of 90 KB (51%); the measured
   worst instant, with the setup AP up, is 54 KB (60%).
3. **Quiet resets** (`power_on`, `software`, `external`) are right, with a caveat: the ESP32
   cannot tell a panic from any other software reset, so today `software` also covers a
   crash-and-restart (a later firmware card adds an RTC breadcrumb so `panic` becomes
   reportable). Until then the honest "it may have crashed" signal is **a change of `boot_id`
   with `reset_reason: software` that the Studio did not ask for**.

The firmware session's card 192 (in flight on their side, `crates/sim`) adds flags and
`SimHandle::set_health(..)` so a simulator can play an unhealthy device; card 180's
acceptance screenshot of the stand-out rows against a device-shaped thing is still owed and
belongs here.

## Deliverables

- Two levels for the stack (`warn` < 8192, `fault` < 4096), one for the heap (> 85%), as
  named flags in `DeviceFacts` - the page still carries no copy of any number.
- "Unexpected reboot": the Studio knows when *it* asked for a reboot (its own reboot
  control). A `boot_id` change with `reset_reason: software` that it did not ask for is
  counted and shown separately from reboots in general ("1 reboot the studio did not ask
  for - it may have crashed"), quiet wording, not a fault: on this bench a reflash looks the
  same, and the page should say so rather than cry wolf. A comment in the code records why
  `software` cannot be trusted to mean "clean".
- Against the simulator with card 192's controls: each stand-out row seen on the page, in a
  real browser, screenshots in the Log (dummy SSID only - never the deployed page).
- No state schema change; additive API only.

## Acceptance

A simulator flipped to stack_free 6000 shows a warning, 3000 a fault, heap 90% a fault, a
brownout reset stands out, and an unasked-for software reboot is named as such; the real
panel at ~20 KB free and 51% heap shows nothing but ordinary rows.

## Log
