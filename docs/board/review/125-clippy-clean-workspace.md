---
id: 125
title: Make `cargo clippy --workspace --all-targets` clean, and keep it clean
type: build
hardware: no
depends: []
owner: worker-176
branch: card/176-125-browse-and-clippy
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

### Done, 2026-09-20 (worker-176)

**Re-counted first.** The card's table is from `main` at dc61607 and a lot has
merged since. `cargo clippy --workspace --all-targets` on `main` at 695e484,
before touching anything:

| crate | warnings | in scope for this card |
|---|---|---|
| `screeny-art` | 15 (10 lib, 4 more in lib tests, 1 in `tests/sender.rs`) | yes, except `src/piece.rs` (clean anyway) |
| `screeny-demos` | 5 | yes |
| `screeny-probe` | 2 | **no** - another session holds it |
| `screeny`, `screeny-proto`, `screeny-encode`, `screeny-panel`, `screeny-receiver` | 0 | - |
| `screeny-studio`, `screeny-sim`, `screeny-settings`, `screeny-provision`, `screeny-device-api` | 0 | out of scope, and already silent |

So the pre-existing noise is narrower than the card expected: only three crates,
and only two of them mine. All thirteen workspace members were linted (checked
against `cargo metadata`); `firmware/` is a separate cargo project on the Xtensa
toolchain and belongs to the firmware session.

By lint, across the 20 in the three crates: seven needs_range_loop, four
`% n == 0` that wanted `is_multiple_of`, two `chunks_exact` with a constant
size, and one each of `let_and_return`, `useless_vec`,
`field_reassign_with_default`, `map_or` -> `is_none_or`, manual
`RangeInclusive::contains`, `should_implement_trait` and `type_complexity`.

**What was left behind, for their owners.** `screeny-probe`, 2 warnings, both
`cargo clippy --fix`-able and both the same lint:

| file:line | lint |
|---|---|
| `crates/probe/src/main.rs:521` | manual implementation of `.is_multiple_of()` - `n.is_multiple_of(30)` |
| `crates/probe/src/vectors.rs:161` | manual implementation of `.is_multiple_of()` - `p.is_multiple_of(2)` |

Nothing is owed by `crates/studio`, `crates/sim`, `crates/settings`,
`crates/provision` or `crates/device-api`: they are already silent.

**Pixel-exactness, checked rather than trusted.** The card's warning about
renderers was the real risk, so before touching `crates/art` I hashed the raw
f32 bits of all 2048 pixels of 40 frames from every piece in `pieces::ALL` at
three seeds - 24 piece/seed pairs, the three GPU pieces included - with a
throwaway example, confirmed the hashes were stable across repeat runs, and
diffed them again afterwards. **Identical.** The harness was deleted; it is four
lines of `Ctx` and a FNV loop and is trivial to write again if another pass
needs it.

`crates/demos` has no golden frames, but its five fixes are all plainly
order-preserving; its 8 tests (including the pixel-count and no-flash ones) stay
green. The blur's inner sum kept its `lo..=hi` order deliberately, with a
comment saying why: it is a float sum and reassociating it would move pixels.

**Judgement calls**, the two the card predicted plus one:

- `Rgb::add` (`crates/art/src/color.rs`) keeps its name and gets
  `#[allow(clippy::should_implement_trait)]` with a reason. It sums light,
  which only means anything because `Rgb` is linear, and `acc.add(..)` says so
  at the call site where `a + b` on a colour would not. (The card guessed
  `Palette::add`; it is `Rgb::add`.)
- `tests/sender.rs`'s `run` returned five parallel vectors. It now returns a
  `Run` struct - the card's own suggestion, and the three call sites read
  better for it - rather than a `type` alias hiding the same tuple.
- New in card 176's code, not pre-existing: `#[allow(clippy::large_enum_variant)]`
  on `discover.rs`'s private `Step`, with a reason.

No workspace lint table entry was needed: every allow is local and local is
right for all three.

**No CI gate** - there is no CI. Instead `docs/README.md` gained a "Clippy"
section: what is expected to be silent, that `screeny-probe` is the exception,
that an `#[allow]` carries a reason, and the float/golden-frame rule.

**Evidence.** `cargo clippy --workspace --all-targets` now prints four lines,
all of them `screeny-probe`. `cargo test --release --workspace --no-fail-fast`
green.
