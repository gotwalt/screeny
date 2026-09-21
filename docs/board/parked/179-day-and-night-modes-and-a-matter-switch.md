---
id: 179
title: Day and night modes, switched from the smart home (a virtual Matter switch)
type: design
hardware: no
depends: [151, 155]
status: parked
owner:
branch:
---

## Goal

The owner, 2026-09-20: "in a future card we'll establish day and night modes, and likely
expose the service as a virtual matter switch for smarthome control. night mode will allow
different brightness/patch, but not a thing i want to do right now."

## Context (for whoever picks this up when he asks)

- This is his answer to the question card 155 left open ("may a night patch, or a named
  setting, ask for a panel brightness?"): **no - brightness and patch belong to a *mode***,
  not to a patch or a setting. A mode is roughly {patch, named setting (card 151), panel
  brightness}; day and night are the two he named. `vesta` is the obvious night patch.
- Switching is from outside: the Studio exposed to the smart home as a **virtual Matter
  switch** (on/off = day/night, or a small set of endpoints), so whatever already runs his
  house decides when it is night. That is not the parked scheduler (card 104, which he does
  not want): there is no timetable inside the Studio.
- Things to find out then, not now: a Rust Matter stack that can run inside the Studio's
  container on workbench (rs-matter's maturity; commissioning from a container with host
  networking; mDNS/DNS-SD for Matter beside the Studio's own browsing), or a simpler bridge
  through the hub he uses; how the panel's brightness cap (firmware: 160) and the Studio's
  brightness policy (`fleet::supervise`) interact with a mode's brightness; what the page
  shows (which mode, and an override).

## Parked 2026-09-20

By the owner's own words: not now. Do not pick up until he asks.

## Log
