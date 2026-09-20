---
id: 186
title: `screeny-probe`'s two clippy warnings, the last in the workspace
type: build
hardware: no
depends: [125]
---

## Goal

`cargo clippy --workspace --all-targets` says nothing at all, rather than
nothing except `screeny-probe`.

## Context

Card 125 made every host crate clippy clean except `crates/probe`, which was
held by another session at the time and was left alone deliberately. What is
left is two occurrences of one lint, and `cargo clippy --fix` writes both:

| file:line | lint | suggestion |
|---|---|---|
| `crates/probe/src/main.rs:521` | manual implementation of `.is_multiple_of()` | `n.is_multiple_of(30)` |
| `crates/probe/src/vectors.rs:161` | manual implementation of `.is_multiple_of()` | `p.is_multiple_of(2)` |

Both are integer tests on a counter, so neither can move a pixel or a timing.

`docs/README.md`'s "Clippy" section names `screeny-probe` as the one exception;
that sentence comes out with this card.

## Deliverables

- The two fixes, no behaviour change.
- The exception removed from `docs/README.md`.

## Acceptance

`cargo clippy --workspace --all-targets` is silent on a clean tree.
`cargo test -p screeny-probe` green.

## Log

## Log

### Orchestrator (2026-09-20)

Done by the firmware session, which owns `crates/probe`: commit 32ff319 fixed the two
`manual is_multiple_of` warnings. The exception sentence in `docs/README.md` is removed; the
workspace has no clippy exception left.
