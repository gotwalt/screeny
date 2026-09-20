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

## Log

**Where the time came from.** `Ctx::now` already was the only clock a piece may read
(`crates/art/src/piece.rs`), and only the two clock pieces read it:
`pieces/clocks/mod.rs:370` (`clock = ctx.now + offset * 60`) and
`pieces/clocks/dials.rs:194` (the same line). Nothing else in `crates/art` touches
`SystemTime`, `chrono` or `Local`; `overland`'s "hour" is a plain parameter, not a wall
clock. So the missing piece was never in the pieces - it was that every runner passed
`local_now()` and there was no way to pass anything else.

**What was built.** `piece::Clock`, a value the runner carries rather than a global:
`Live` (read the machine) or `Pinned(seconds)` (pretend it was that time of day at
engine time zero and run on with engine time). One method, `now(t)`. `Clock::parse`
takes `HH:MM` or `HH:MM:SS`, seconds may be fractional. The studio can hold one later
without anything here changing.

**The day is day zero, not today.** The obvious reading of "pretend it is 21:12" is
today's midnight plus 21:12, and it would have been wrong: the numerals piece seeds
each minute's choreography from the *absolute* minute number
(`self.seed ^ (next as u64).wrapping_mul(0x51ed_270b)`, and `dance_for(next, ..)`), so
"21:12 today" would choose a different dance tomorrow. Pinning to day zero costs
nothing - no piece reads the date - and makes the command mean one thing for ever.

**`snapshot::take`.** The snapshot loop moved out of the binary into
`crates/art/src/snapshot.rs` so the tests render through the code the command runs
rather than a copy of it. The arithmetic is unchanged, `began + (t - first)` literally,
with only `began` now coming from the clock; for `Clock::Live` that is `local_now()`, as
before.

**Pixel-exactness, measured.** Built `screeny-art` from the pre-change commit (6ea8a3c)
into a separate target dir and rendered nine snapshots - plasma, metaballs, testcard at
`--at 3/6/20`, `--seed 7`, full warmup - with both binaries. All nine SHA-256s identical
(e.g. plasma-6 `01a47dbed917f57b...`, metaballs-20 `a29dd971cc0facc6...`, testcard-3
`2766a2c660b4733e...`). GPU pieces were out of that build (`--no-default-features`) and
do not read the clock.

**The one obvious command.** `--at` defaulting to 5 s caught the numerals piece
mid-dance, so the card's acceptance would have failed as literally written. `--time` now
also moves `--at` to 20 s (and `--warmup` to the whole run; either given explicitly still
wins). 20 s is measured: over 60 seeds the opening dance onto the born-on minute is
8.9-15.4 s, and over all thirteen named choreographies at four seeds each 7.6-15.4 s.
The next dance does not set off until ~45 s. So:

| picture | command | measured |
|---|---|---|
| numerals settled on 21:12 | `--time 21:12` | `e02114173eb0819c...` for all 60 seeds, for all 13 named dances at 4 seeds each, and for `--at 40 --set still=60` |
| numerals 2 s from landing on 21:12 | `--time 21:11:20 --at 38 --seed 7` | `4d4551de149b258e...`, twice |
| dials telling 10:10 | `--time 10:09:50 --at 15` | `f1d9b5f039ff167a...` for seeds 1/7/42/999, and twice minutes apart |

Two runs of the acceptance command, with different random seeds and four seconds apart
on the wall clock, gave the same PNG as a run twenty minutes earlier. Checked the
pictures by eye too: the settled one reads `21:12`, the dials at 02:59:50+15 s have every
minute hand at 12 and every hour hand at 3.

**Tests.** `crates/art/tests/pinned_time.rs`, four, all through `snapshot::take`: the
same pinned shot rendered 1.1 s apart in real time is byte-identical for both clock
pieces; two different pinned times differ, so the flag really reaches the piece; the
acceptance command's shot is one picture over four seeds and all thirteen dances; and a
piece that never reads `Ctx::now` does not notice `--time`. Plus three unit tests for
`Clock::parse` in `piece.rs`, including that a pinned time is a number under 86400.

**Handover checks.** `cargo clippy --workspace --all-targets` says nothing.
`cargo test --release --no-fail-fast` at the root: 81 test targets `ok`, one FAILED -
`screeny-studio --test soak`, one of the two card-117 flakes, and another worktree was
running its own soak in a five-run loop at the time. Re-run alone once that loop had
finished: `test result: ok. 1 passed; 0 failed; ... finished in 60.95s`. Not touched;
card 117 owns it. `crates/screeny/tests/embed.rs`, the other known flake, passed in the
full run.

**Out of scope, left alone.** The pieces' `offset` parameter is untouched. Nothing
outside `crates/art` was changed: `crates/studio/src/player.rs` still builds its own
`Ctx { now: local_now(), .. }` and compiles unchanged, because `Ctx` gained no field.

### Orchestrator, after the merge (2026-09-20)

Reviewed (art-only, `Ctx` unchanged, so the Studio compiles untouched) and merged `--no-ff`.
`cargo test --release -p screeny-art`: 63 + 4 + 3 pass; clippy silent; the full root suite
runs with the rest of this batch. Ran the acceptance command twice with random seeds:
the same PNG hash both times, and it reads 21:12. First real use, minutes later: the same
mid-dance frame of `clocks-numerals` at `tip=1` and `tip=0.45` for the owner (card 115).
