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
