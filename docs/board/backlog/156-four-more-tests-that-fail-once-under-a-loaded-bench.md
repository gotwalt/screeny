---
id: 156
title: Four more tests that each failed once under a loaded full suite
type: test
hardware: no
depends: [117]
owner:
branch:
---

## Goal

Card 117's treatment for the rest of the family: make each say its numbers when it fails,
wait on what it asserts, and bound it on the code's own schedule, not the OS scheduler's.

## Context

Reported by the firmware session on 2026-09-20: under a loaded full `cargo test` (two
Claude sessions and their workers build and test on this one Mac all day), each of these
failed once and passed alone:

- `screeny-studio` `tests/moved.rs` (card 141's two-simulator test; it re-binds ports)
- `screeny-studio` `tests/soak.rs` - check first whether that run had card 117's fix
  (`sim_again()`, `flowing()`); it may already be dealt with
- `screeny` `tests/pacing.rs::holds_thirty_fps_within_one_percent` - card 093 rebuilt this
  file around the pacer's own wake-ups; a 1% wall-clock bound may still be one too many
- `screeny` `tests/loopback.rs::the_reported_rate_is_the_same_at_any_stream_length`

Nobody captured a failure message. Until this is done the rule in `docs/ORCHESTRATOR.md`
holds: a single failure in one of these under a loaded bench is re-run alone before it is
believed, and both results are reported.

## Deliverables

Each test either shown to have a cause (fixed) or made to print what it measured; 10 runs
alone and one under a cold `cargo build --release` into a scratch target dir.

## Acceptance

Three full-suite runs in a row under load with none of the four failing, or a failure that
says enough to fix.

## Log
