---
id: 113
title: Show the sender's cadence moving, not just its current rate
type: build
hardware: no
depends: [102]
owner:
branch:
---

## Goal

A person watching the studio on a bad WiFi day should be able to see the link
stepping its rate down and back up. Today they cannot tell that apart from
somebody having moved the frame-rate slider.

## Context

Written by card 102, which was asked to write down what the sender does at 60
vs 30 fps and to change nothing. What it does:

- The player renders at `fps` (1..60, default 60, card 172) and hands every
  frame to `Link::send`, which never sleeps.
- `Cadence::Limit` (the default) drops frames that arrive before the next slot
  on an absolute schedule, at the rate the link is currently targeting. That
  rate starts at `SenderConfig::fps` and follows spec 6.9's ladder down under
  loss and back up. A 60 fps piece into a 30 fps panel puts 30 on the wire and
  the device supersedes nothing; half the frames come back `Sent::Coalesced`,
  which is the system working.
- The page's Panel block shows `Frames  N sent, M folded, K lost`
  (`PanelStatus::frames_sent` / `frames_coalesced` / `frames_dropped`),
  `Link  up · N fps` (`PanelStatus::fps`) and `Rendered  N frames at M fps`.

What is missing:

- **No history.** `PanelStatus::fps` is the current target and nothing else.
  A step down under sustained loss, and the step back up when it clears, are
  invisible: the number simply reads lower for a while, exactly as it would if
  the piece had been set slower. The ladder is the most interesting thing the
  link does and it is the one thing not on the page.
- `PanelStatus::frames_offered` is carried and never displayed. Cosmetic - it
  is sent + folded + lost - but either show it or say why not.

## Deliverables

Whatever is smallest that makes the ladder legible. A sketch, not a decision:
a `cadence_changes` counter and a `last_cadence_change` timestamp on
`PanelStatus`, so the page can say "stepped to 20 fps 14 s ago"; or a short
ring of (time, rate) in the link. `crates/screeny/src/embed.rs` owns the
ladder; `crates/art/src/output/sender.rs` owns `PanelStatus`; the page is
`crates/studio/ui/main.js`.

## Acceptance

With a simulator made to drop frames, the page says the rate stepped down and
then says it stepped back up, and a person can tell that from the slider
having been moved. No new per-frame logging.

## Log
