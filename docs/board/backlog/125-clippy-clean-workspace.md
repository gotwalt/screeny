---
id: 125
title: Make `cargo clippy --workspace --all-targets` clean, and keep it clean
type: build
hardware: no
depends: []
owner:
branch:
---

## Goal

One command that says nothing, so that a warning a new change introduces is
visible instead of being the 22nd line of noise.

## Context

Found while doing cards 111 and 112, which needed clippy to be readable to tell
whether their own code was clean (it was - the one new warning, a
`large_enum_variant` on `embed.rs`'s private `Aim`, was allowed with a reason in
the commit that caused it). Everything else is pre-existing, and none of it is
in `crates/screeny`, `crates/proto` or `crates/encode`, which are already quiet.

`cargo clippy --workspace --all-targets` on `main` at dc61607:

| crate | warnings |
|---|---|
| `screeny-art` (lib) | 10, of which clippy will fix 7 |
| `screeny-art` (tests) | 4 more, plus one "very complex type" on `tests/sender.rs`'s `run` |
| `screeny-demos` | 5, of which clippy will fix 1 |
| `screeny-probe` | 2, both fixable |

By lint: six `manual implementation of .is_multiple_of()`, five "loop variable
used to index", two `chunks_exact` with a constant size, and one each of
`map_or` -> `is_none_or`, `useless vec!`, `let_and_return`,
`should_implement_trait` (a `Palette::add`), `manual RangeInclusive::contains`,
`field_reassign_with_default`, and `type_complexity`.

Almost all of it is `cargo clippy --fix`. The judgement calls are the few that
are not: the `type_complexity` on a test helper that returns five parallel
vectors (a struct would read better than a `type` alias), and
`should_implement_trait` on `Palette::add`, which may simply deserve an
`#[allow]` with a reason - "it takes an OKLCH colour and returns an index, which
is not addition".

Not urgent, and not something to do while four workers are in flight: it touches
`crates/art`, `crates/demos` and `crates/probe`, which is a merge conflict with
whoever holds those. Best done alone, in one pass, on a quiet board.

## Deliverables

- `cargo clippy --workspace --all-targets` with no output.
- Any `#[allow]` that survives carries a reason in a comment beside it, the way
  `embed.rs`'s two do.
- If a lint is judged not worth obeying workspace-wide, say so once in the root
  `Cargo.toml`'s lint table rather than sprinkling `#[allow]`s.

## Acceptance

- The command above is silent, on a clean tree, for all four crates.
- No behaviour changed: every crate's tests still pass, and the diff is
  readable as "the same code, said better".

## Log
