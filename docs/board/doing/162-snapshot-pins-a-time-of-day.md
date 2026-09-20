---
id: 162
title: screeny-art snapshot cannot pin a time of day
type: build
hardware: no
depends: [100]
owner: worker (Opus)
branch: card/162-snapshot-pins-a-time
---

## Goal

`screeny-art snapshot` should be able to say *what time it is* for the pieces that tell
the time, so a picture of `clocks-numerals` at 21:12 is one command and the same command
tomorrow.

## Context

Found while building card 160's contact sheet. `snapshot` simulates the time of day
(`local_now()` at the first frame, advancing with `--at`/`--warmup`), and the only way to
aim it at a chosen time is the piece's own `offset` parameter, in minutes *relative to
now*. So the sheet was built by a script that read the wall clock, worked out a
**fractional** offset that puts the simulated clock on the target minute's boundary at
the start of the warmup, and chose a warmup long enough for the first dance to land but
short enough that the next one has not set off
(`docs/research/010-numerals-rest-pose.md`, "How it was rendered"). That works, and it is
not something the next person should have to rediscover.

It is also why card 160's "snapshot tests for the awkward times" are pose-and-ink
assertions in the piece's own tests rather than end-to-end renders: a test cannot ask the
binary for 21:12.

## Deliverables

- `screeny-art snapshot --time HH:MM[:SS]` (and `pipe`, which has the same problem):
  sets the simulated time of day at `--at`, whatever the wall clock says. `--at`,
  `--warmup` and `--seed` keep their meanings, so a run stays reproducible.
- The clock pieces' `offset` parameter stays what it is: an offset, for the studio.
- Say in `crates/art/README.md` how to get a picture of a chosen time.

## Acceptance

`screeny-art snapshot clocks-numerals --time 21:12 --out x.png` draws 21:12, twice in a
row, at any hour of the day, with the hands holding the time rather than mid-dance.
