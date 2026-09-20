---
id: 092
title: An indexed FrameSource for the built-in demos, and four cargo doc warnings
type: build
hardware: no
depends: [011, 016]
owner: worker-092
branch: card/092-indexed-framesource
---

## Goal

Two small pieces of `crates/screeny` tidying that card 011 deliberately left
alone, because doing them needs `crates/demos`, which card 016 held at the
time.

## Context

Card 011 added `Pixels`, `Sender::send_indexed` and `Link`, so an *embedder*
can put an indexed frame on the wire exactly. The built-in demos cannot: they
are `FrameSource`s, which render into an RGB `Frame`, so the word clock - eight
colours, drawn from a palette it already has - is expanded to 6144 bytes and
then has its palette rediscovered by the encoder's histogram one frame later.
The picture is identical (the ladder finds the same colours and `PAL4_LZ`
carries them losslessly), so this is about honesty and about 0.05 ms, not about
quality.

Card 011's note: adding the seam without a consumer would have been dead API,
and the only natural consumer is `crates/demos`, which was being refactored in
parallel.

Separately, `cargo doc -p screeny --no-deps` emits four warnings, all
pre-existing and all the same mistake: `crates/screeny/src/encode/quant.rs`
links `[`SEED_DRIFT`]` and `[`RESEED_EVERY`]` from public documentation, and
both constants are private (lines 23, 29, 154, 155).

## Deliverables

- An indexed variant of the `FrameSource` seam - most likely a second trait
  (`IndexedSource`) rather than a method on `FrameSource`, so no existing
  implementor changes - and a `Sender`/`Link` path that takes it.
- `crates/demos`' word clock implemented against it, since it is already
  palette-authored. The fractal is a continuous-colour renderer and should stay
  RGB.
- A test that the clock's frames arrive bit-exact through `screeny-sim` and
  that the encoder never sees an expanded frame (assert on
  `SendStats::indexed_exact`).
- `quant.rs`: either make the two constants public, or unlink them. They are
  genuinely part of the explanation, so public is probably right.

## Acceptance

`cargo test -p screeny -p screeny-demos` passes; `cargo doc -p screeny
--no-deps` is warning-free; `screeny clock` sends `PAL4_LZ` with
`indexed_exact` rising and `indexed_fallback` at zero.

## Log

### Claimed (worker-092)

Branch `card/092-indexed-framesource` off `main` at c326521.

Where the card's Context has drifted since it was written (cards 016, 011,
101, 111):

- `quant.rs` is **`crates/encode/src/quant.rs`** now, not
  `crates/screeny/src/encode/quant.rs` - card 016 moved the encoders into
  `screeny-encode`. So `cargo doc -p screeny --no-deps` is *already* silent
  (`--no-deps` skips the dependency that owns the file); the four warnings are
  on `cargo doc -p screeny-encode --no-deps`, at quant.rs:23, 29, 154, 155,
  exactly as described. Both spellings will be checked.
- `Link` needs no new door: `Link::send(Pixels)` has taken
  `Pixels::Indexed` since card 011, and `Link` is push-only (no pull loop to
  add a source to). The missing half is the **pull** model, `Sender::run`,
  which is what the CLI's `screeny clock` uses.
- `crates/demos` already renders the clock as palette + indices:
  `Piece::render_indexed(&mut Indexed) -> bool`, `WordClock` overrides it,
  `FractalZoom` does not. Nothing consumes it. The expansion the card is about
  is in `main.rs`'s `PieceSource`, which calls `Piece::render` (which does
  `Indexed::to_frame()`) and then copies 6144 bytes into the sender's frame.

### The four doc warnings

`SEED_DRIFT` and `RESEED_EVERY` in `crates/encode/src/quant.rs` are now `pub`,
which is the option the card preferred: both are named in the public prose of
the `quant` module and of `quant::build`, and the numbers - 1.25 and 30 frames
- are the whole of the "is the seeded palette still good?" rule. A reader of
those docs who cannot see them is reading a sentence with a hole in it.

Before: `cargo doc -p screeny-encode --no-deps` = 4 warnings
(`rustdoc::private_intra_doc_links`, quant.rs:23, 29, 154, 155).
After: `cargo doc -p screeny-encode -p screeny --no-deps` = silent.

### The seam: `IndexedSource`, and one pacing loop

`crates/screeny/src/frame.rs`:

```rust
pub trait IndexedSource {
    fn render_indexed(&mut self, t: FrameTime) -> Option<Pixels<'_>>;
    fn name(&self) -> &str { "frames" }
}
```

Three decisions worth recording.

1. **A second trait, not a method on `FrameSource`.** The card suggested it and
   it is right: no existing implementor changes, and the two are genuinely
   different shapes - one renders *into* a buffer the sender owns, the other
   *lends* buffers it owns itself.
2. **It returns `Option<Pixels<'_>>` rather than filling an
   `&mut SomeOwnedIndexedFrame`.** That is what stops this card adding a
   fourth owned "palette + indices" type to a tree that already has three
   (proto's borrowed `IndexedFrame`, `screeny_demos::Indexed`,
   `art::Frame::Indexed`). The source keeps its own buffers - which every
   producer already does - and hands out a borrow, so the public surface grows
   by one trait and nothing else, and the frame lands in `Pixels::indexed` /
   `Sender::send_indexed` exactly as the card's "one implementation" rule
   demands. `None` ends the stream, the way `false` does for `FrameSource`.
   Returning `Pixels::Rgb` is allowed and documented: it takes the ordinary
   chooser, which is what a source that is only *sometimes* palette-authored
   wants - and that is precisely the fractal.
3. **One pacing loop, not two.** `Sender::run_with` and the new
   `Sender::run_indexed_with` both call a private `Sender::drive`, whose body
   is the old loop with `src.render(t, &mut frame); self.send_frame(..)`
   replaced by `src.produce(t, self)?` on a private `Driver` trait (two
   implementors, eight lines each). Copying spec 9.1's schedule into a second
   function to serve a second seam would have been the real duplication. **Card
   093 note:** none of the timing changed - `sleep_until`, `period_of`, the
   absolute schedule and the skip-never-burst arithmetic are byte for byte
   what they were; only the two lines that produce a frame moved.

`Link` needed nothing, as recorded above: it is push-only and
`Link::send(Pixels::Indexed { .. })` is already the exact path.
