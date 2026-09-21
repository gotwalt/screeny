---
id: 114
title: The simulator's window draws the panel without its temporal dither
type: build
hardware: no
depends: [102]
owner:
branch:
---

## Goal

`cargo run -p screeny-sim` should show what the panel shows. Since card 102 it
does not agree with the studio's preview of the same frame in the dark end.

## Context

Found by card 102, which did not touch `crates/sim`.

`crates/sim/src/panel.rs`'s `panel_of` maps the simulator's `PanelModel { levels }`
knob onto `screeny_panel::Panel::levels(n)`, and the default is 64. That is
`screeny_panel::NOMINAL`: the bit planes alone, 22 of the 256 sRGB codes crushed
to black, darkest visible sRGB 34. The device has not looked like that since
card 030 - it spends each colour's sub-level remainder across sixteen refreshes,
so a held colour resolves 1008 duty steps and only sRGB 0 and 1 are black
(`screeny_panel::DEVICE`, pinned to the firmware's own gamma table).

So the simulator's **window** is pessimistic about the darks by about 30 sRGB
codes, while `crates/art`'s preview and `crates/studio` are now right. Two
pictures of one panel that disagree is exactly what card 016 set out to stop.

Not affected: `Snapshot::decoded`, which is bit-exact and is what every test
in the crate reads, including `crates/art/tests/sender.rs`. This is the window
and the PNG, not the protocol.

## Deliverables

`crates/sim/src/panel.rs`: the default panel becomes `screeny_panel::DEVICE`,
and the `levels` configuration knob either gains a way to ask for the
undithered panel (the comparison is worth keeping) or is replaced by one, in
the same two-way shape as `screeny_art::panel::Panel`. The three tests in that
file assert 64 levels and will need to say what they now mean.

Worth doing at the same time: `crates/demos` also scores against `NOMINAL`
(`crates/demos/tests/clock.rs`), which may or may not be the right model for
what it measures - check rather than assume.

## Acceptance

The simulator's window and `crates/studio`'s preview of the same frame are the
same picture, checked by a test that renders one frame both ways and compares
bytes. `screeny-sim`'s own protocol tests are untouched.

## Log

### Parked 2026-09-21 (owner: "I don't need to over-build this"; the three that matter are 188, 187, 199)

Nobody judges the picture in the simulator's window: the Studio's preview and the panel are both right since card 102. Reopen if the sim's window or its PNGs are ever used as evidence of the dark end. Card 188 adds to crates/panel; read its result first.
