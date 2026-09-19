---
id: 067
title: Let a Piece render straight into the sender's frame buffer
type: build
hardware: no
depends: [016, 011]
---

## Goal

Delete the 6 KB copy `PieceSource` makes on every frame.

## Context - found while doing card 016

Card 016 was asked to make `demos` use proto's frame types "so that
`PieceSource` in `crates/screeny/src/main.rs` stops copying 6 KB per frame". It
did the first half: `demos::Frame` is now a `Box<Rgb888Frame>` and the geometry
constants are proto's. The copy is still there:

```rust
fn render(&mut self, t: FrameTime, out: &mut Frame) -> bool {
    self.piece.render(t.elapsed, &mut self.scratch);
    out.as_bytes_mut().copy_from_slice(&self.scratch.px[..]);
    true
}
```

It could not be removed inside card 016's scope, and the reason is worth
recording because it decides how this card should be done:

* `Piece::render` takes `&mut demos::Frame`, an **owning** type. To render
  into the sender's buffer the piece needs a **borrowed** frame
  (`struct FrameMut<'a>(&'a mut Rgb888Frame)`, or `Piece::render` taking
  `&mut Rgb888Frame` directly), which touches every renderer in
  `crates/demos` - fractal.rs and clock.rs are 1100 lines between them - and
  every caller in `preview`.
* The alternative, swapping the two boxes instead of copying, needs
  `screeny::Frame` to hand out its `Box`. That file belonged to card 011.

Neither is hard; both are wider than a consolidation card should reach, and
card 011 was in flight in the same files.

Worth being honest about the size of the prize: 6 KB at 30 fps is 184 kB/s and
a few microseconds a frame on a laptop. Do this for the shape, not the clock -
the shape is that a renderer should write where the encoder will read.

## Deliverables

- `Piece::render` renders into a borrowed 64x32 sRGB888 frame.
- `PieceSource::render` passes the sender's buffer straight through; no
  `scratch` field.
- `crates/demos`' preview and tests updated; no behaviour change in any
  rendered frame (the golden-ish tests in `demos/tests/` are the check).

## Acceptance

`grep -n copy_from_slice crates/screeny/src/main.rs` finds nothing in
`PieceSource`, and `cargo test --workspace` is green with the same assertions.
