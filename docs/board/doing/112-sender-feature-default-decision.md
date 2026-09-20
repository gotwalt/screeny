---
id: 112
title: Decide whether screeny-art's `sender` feature is on by default
type: design
hardware: no
depends: [101]
owner: worker-111
branch: card/111-112-link-attach-sender-default
---

## Goal

Settle one question and act on it: should a plain `cargo test` at the workspace root
run the art system's on-the-wire acceptance, or not?

## Context

Card 101 put `SenderOutput` behind a non-default `sender` feature, because the card
asked for the core to build with no network stack and for both configurations to be
checked. A consequence nobody chose: `crates/art/tests/sender.rs` is
`#![cfg(feature = "sender")]`, so a plain `cargo test` prints

```
Running tests/sender.rs ... test result: ok. 0 passed; 0 failed
```

The three tests that prove indexed frames arrive pixel-exact and that the preview is
what the panel shows are simply not run. A regression in `Link`, in the chooser or in
the pipeline would pass CI. The tests exist and are fast (about 2 s); the only thing
stopping them is a feature flag.

The tension is real on both sides:

- **For default-on.** `gpu` is already default-on in the same crate with
  `--no-default-features` as the escape, so `sender` being different is a surprise.
  The acceptance would run by default, which is the point of having it.
- **For default-off.** Every build of `screeny-art` would then link mdns-sd, ctrlc and
  the sockets, including `screeny-art pipe` and `snapshot`, which want none of them.
  On macOS a binary that *can* talk to the LAN is a binary that may be asked about
  Local Network permission (card 110).

Options worth weighing, not only the two obvious ones:

1. `sender` default-on, `--no-default-features` for the network-free build.
2. Keep it off and make the workspace's test command include it - a `cargo xtask`, a
   `.cargo/config.toml` alias, or a line in `CLAUDE.md` and `docs/README.md`. Cheap,
   and it only works if people read it.
3. Split the binary: `screeny-art` stays network-free, `screeny-art-play` (or the
   studio's server, card 105) owns the link. Heaviest, and closest to how
   `crates/screeny` justifies being one binary on purpose (card 015).

Card 105 turns the studio into a server that must have the sender, so whatever is
decided should still make sense then.

## Deliverables

- The decision, written down where the next person finds it (`crates/art/README.md`
  and, if it changes how the workspace is tested, `CLAUDE.md`'s ground rules).
- The change itself.

## Acceptance

- The wire acceptance in `crates/art/tests/sender.rs` runs in whatever command the
  project calls "the tests", with no extra flags to remember.
- A build with no network stack still exists and is named in the README.

## Log

### 2026-09-19 - the decision (taken by the orchestrator)

**Option 1: `sender` is default-on in `screeny-art`. `--no-default-features` (or
`--no-default-features --features gpu`) is the network-free build.** The
orchestrator made this call and handed it to this worker to carry out; the reasons
given, recorded here so the next person does not have to re-derive them:

- The art system is now the project's **primary sender**. A build of it that cannot
  send is the special case, and special cases belong behind a flag.
- `gpu` is already default-on in the same crate with the same escape hatch, so a
  second feature behaving differently is a trap rather than a choice.
- The wire acceptance in `tests/sender.rs` - the *only* check that indexed frames
  reach a panel pixel-exact, against the independent `screeny-sim` decoder - must run
  in a plain `cargo test`. Off by default it printed `0 passed` and a regression in
  `Link`, in the chooser or in the pipeline would have gone through green.
- The trap had already sprung: a plain `cargo test --release` at the root rebuilt
  `target/release/screeny-art` **without `play`**, which bit the orchestrator during
  card 101's panel run.

The counter-argument from the card (every `screeny-art pipe`/`snapshot` build then
links mdns-sd, ctrlc and sockets, and on macOS a binary that *can* reach the LAN may
be asked about Local Network permission, card 110) is answered by the escape hatch
being real and named, not by the default. Measured: `cargo tree -p screeny-art
--no-default-features -e normal` is five direct dependencies (libc, png,
screeny-encode, screeny-proto, serde) and **zero** mentions of mdns-sd; the default
build adds `screeny`, ctrlc, wgpu and friends.

### 2026-09-19 - the change

- `crates/art/Cargo.toml`: `default = ["gpu", "sender"]`, with the reasoning and the
  network-free build written into the comment above the feature.
- `crates/art/README.md`: a new "The `sender` feature is on by default" section under
  "Sending to a panel" - the decision, dated and attributed, the four reasons, and the
  network-free build named twice (there and in the "Run" block, which no longer tells
  people to pass `--features sender` for `play` or for the tests).
- Doc comments that the flip made untrue: `src/lib.rs` (the crate header),
  `src/output/mod.rs`, `src/output/sender.rs` (it claimed the feature being off was
  "what the headless runner and the tests want" - the tests want the opposite), and
  both strings in `src/bin/screeny-art.rs` that told a `--no-default-features` user to
  pass `--features sender`. They now say the feature is on by default and that this
  binary was built without it.
- `CLAUDE.md`: **nothing to fix.** Its only sentence about the workspace tests is "A
  plain `cargo test` skips only `crates/studio` (Tauri, until card 105)", which is
  still exactly true - and more true than before, since the art wire tests now run.
  `docs/design/generative-art-brief.md` says `SenderOutput` is "behind the `sender`
  feature", which also remains true; left alone (design docs are the orchestrator's).
  `docs/board/ROADMAP.md`'s card 101 line records what card 101 did and is history,
  not a claim about today; left alone.
- `crates/studio/Cargo.toml` (card 105's crate, not touched) depends on
  `screeny-art` with `features = ["sender"]` explicitly, which is still correct and
  now redundant - harmless either way.

### 2026-09-19 - acceptance, measured

- Plain root `cargo test --release --no-fail-fast`: **green**, and it now runs the
  wire acceptance - `Running tests/sender.rs ... test result: ok. 3 passed; 0 failed;
  0 ignored; 0 measured; 0 filtered out; finished in 2.05s`. (Before this card that
  line read `0 passed`.)
- `cargo build -p screeny-art --no-default-features`: builds, 3.85 s.
  `--no-default-features --features gpu`: builds, 26.01 s.
- `cargo test -p screeny-art --no-default-features`: 30 passed, 0 failed;
  `tests/sender.rs` compiles to 0 tests there, which is what the `cfg` is for.
- `cargo run -p screeny-art --no-default-features -- play plasma` still refuses with
  "built with --no-default-features, so the `sender` feature is off and there is no
  network stack in this binary".
- After that same plain root `cargo test --release`, `target/release/screeny-art play
  --help` prints the usage block **with** the `play` line in it: the thing that went
  wrong in card 101 cannot happen again.
