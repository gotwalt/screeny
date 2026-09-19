# Project docs

## Layout

```
docs/
  board/            kanban: one markdown file per card
    backlog/        ready to be picked up (ordered by card number)
    doing/          claimed; the card names its owner and branch
    review/         work finished on a branch, waiting for the orchestrator to merge
    done/           merged to main
  research/         findings. One file per question, conclusions first.
  design/           settled decisions (protocol spec, architecture). Source of truth.
```

## Cards

File name: `NNN-short-slug.md`. Front matter:

```
---
id: 007
title: Short imperative title
type: research | design | build | test
hardware: no | yes        # yes = may touch serial port / camera / flash
depends: [003, 004]
owner:                    # filled in when claimed
branch:                   # filled in when claimed
---
```

Body: **Goal**, **Context** (links into research/ and design/), **Deliverables**
(exact file paths), **Acceptance** (how we know it is done), and a **Log** section
the worker appends to.

## Worker protocol

1. You are given a card number. Work only in your own git worktree on branch
   `card/NNN-slug`.
2. First commit: `git mv` the card from `backlog/` to `doing/`, fill in `owner`/`branch`.
3. Do the work. Commit early and often. Keep to the card's scope; if you find
   other work, write a new card into `backlog/` rather than doing it.
4. Append what you did, what you measured, and anything surprising to the card's **Log**.
5. Last commit: `git mv` the card to `review/`. Do not merge to `main`.
6. Report back: branch name, summary, open questions.

The orchestrator reviews, merges to `main`, and moves the card to `done/`.

## Hardware access

One Tidbyt, one serial port, one camera. Parallel flashing or capture corrupts
results, so:

- Only cards with `hardware: yes` may touch `/dev/cu.usbserial-2140` or the camera,
  and only one such card is in `doing/` at a time.
- Everyone else develops against the host simulator (`sim`), which speaks the same
  wire protocol as the firmware.
- Serial baud <= 230400. Higher rates corrupt data on this bench.
- No flashing without a verified stock backup in `backup/`.
