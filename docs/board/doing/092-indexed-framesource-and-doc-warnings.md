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

### The demos, and what the CLI now does

`crates/demos` could not implement the trait even if it wanted to: `screeny`
depends on `screeny-demos`, so the arrow cannot point back. The bridge is
`PieceSource` in `crates/screeny/src/main.rs`, which now implements
`IndexedSource` instead of `FrameSource` and serves **both** pieces:

```rust
fn render_indexed(&mut self, t: FrameTime) -> Option<Pixels<'_>> {
    if self.piece.render_indexed(t.elapsed, &mut self.idx) {
        Some(Pixels::indexed(&self.idx.palette, &self.idx.indices[..]))
    } else {
        self.piece.render(t.elapsed, &mut self.rgb);
        Some(Pixels::rgb(self.rgb.as_bytes()))
    }
}
```

`Piece::render_indexed` already returned "did I author a palette?" (card 010 /
016) and `WordClock` already overrode it; nothing in `crates/demos` needed to
change, which is why that crate's diff is empty. The clock takes the first
branch and the fractal the second, so no probe and no capability flag was
needed: the bool the trait already returns *is* the capability, answered per
frame.

Two things fell out of that:

- The 6144-byte `copy_from_slice` per frame is gone for both pieces - a piece's
  own buffer is lent straight to the encoder. That is **card 067's prize**,
  reached from the other end; 067's actual deliverable (`Piece::render` taking
  a borrowed frame) is still undone, and I have not touched its card.
- `screeny fractal` now goes through `Sender::send` (RGB arm) rather than
  `Sender::send_frame`. Same encoder, same accounting - `deliver` and
  `send_frame` both `record_encode` then `transmit` - plus one `Pixels::check`,
  which is a length comparison.

`stream_source` grew a sibling, `stream_indexed`, both over a private `Src`
enum, so the resolve/connect/stats-printing block is written once.

The summary line now says the exactness claim out loud when a stream used the
indexed door:

```
20 indexed frames exact on the wire
```

and, if any frame had to be requantised, how many and how big its palette was.
That is the line to read after `screeny clock` against the real panel.

### The tests

`crates/screeny/tests/indexed.rs` (the file that already owns the exactness
claim, checked by `screeny-sim` - the other implementation of the protocol):

- `the_word_clock_arrives_exactly_at_every_kind_of_moment` - six awkward
  moments (settled phrase, the turnover minute, mid-roll with sub-pixel
  offsets, NOON, MIDNIGHT, a three-line phrase). Each one asserts the
  simulator's decoded 6144 bytes equal the clock's own `palette[index]`, that
  the codec was `PAL4_LZ`, and `indexed_exact == 1`, `indexed_fallback == 0`.
- `streaming_the_word_clock_is_exact_for_every_frame` - twenty paced frames
  through `Sender::run_indexed`: 21 sent (20 + `FINAL`), `indexed_exact == 20`,
  `indexed_fallback == 0`, `PAL4_LZ` for all 21, and the frame the simulator is
  showing (located by its sequence number) is bit-identical to the one the
  source produced.

Both use a `ClockSource` that counts every use of the **RGB** branch. That
counter being zero is the card's "the encoder never sees an expanded frame",
stated as an assertion rather than as a hope.

`crates/screeny/tests/cli.rs::clock_streams_its_own_palette_exactly` runs the
real binary - `screeny clock --at 11:42:50 --addr 127.0.0.1:PORT --fps 30
--duration 1` - against the loopback receiver and checks every arriving frame
is `PAL4_LZ` with 11 colours or fewer, that stdout says `pal4-lz 100%` and
`N indexed frames exact on the wire`, and that it never says `requantised`.

### Surprise: the fractal is palette-authored too, and gains more than the clock

The card says "the fractal is a continuous-colour renderer and should stay
RGB". That was true when the card was written; it is not true of today's
`crates/demos`. `FractalZoom` implements `Piece::render_indexed` (fractal.rs
line 540): it renders in linear light, picks the two nearest entries of a
32-ish-entry ramp and dithers along the line between them, and
`demos/tests/fractal.rs::the_indexed_path_is_always_exact` already pins that.
So the one adapter hands *both* pieces to `send_indexed`, and I did not
special-case the fractal back onto the RGB path: doing so would mean writing
code to refuse a door the piece already implements.

Measured, release, against `screeny-sim` on 127.0.0.1 (this laptop was
building firmware and a docker image for two other workers at the time, so the
millisecond figures are pessimistic):

| | bytes/frame | encode mean | codec | exact | README claimed before |
|---|---|---|---|---|---|
| `screeny clock`, 10 s | 271 | 0.07 ms | `PAL4_LZ` 100% | 301/301 | ~280 B, 0.05 ms |
| `screeny fractal`, 4 s | 753 | 0.16 ms | `PAL8_LZ` 100% | 122/122 | ~1300 B, 0.9 ms |

Both a steady 30 fps, device counters `drop 0/0/0` throughout. The pixels are
unchanged in both cases - the old path was lossless too, the ladder having
rediscovered the piece's own palette from the expanded frame - so this is the
same picture at 58% of the bytes and a fifth of the encode time for the
fractal. If the orchestrator would rather the fractal stayed on the RGB path,
it is a two-line change in `PieceSource`; I think the measurement argues
against it.

### Evidence

```
$ ./target/release/screeny-sim --headless --no-mdns \
      --frame-port 49574 --control-port 49575 --exit-after 25 &
$ ./target/release/screeny --addr 127.0.0.1:49574 clock \
      --at 11:42:50 --fps 30 --duration 10
streaming clock to screeny sim at 127.0.0.1:49574 at 30.0 fps, budget 1464 B
     31 frames   31.0 fps     253 B  enc  0.08/ 0.50 ms  [pal4-lz 100%]  dev: shown 1 drop 0/0/0
    ...
    301 frames   30.0 fps     271 B  enc  0.07/ 0.50 ms  [pal4-lz 100%]  dev: shown 277 drop 0/0/0
sent 302 frames in 10.0 s (30.19 fps), 0 skipped, 271 B mean, encode mean 0.07 ms / p95 0.50 ms / max 0.17 ms
301 indexed frames exact on the wire
```

The simulator's own log agrees: `PAL4_LZ`, `rx 302 shown 301`, `stale 0 super 0
dec 0 rej 0`. (One oddity, unrelated: after my stream ended and the sim had
gone back to `IDLE`, its counters picked up another 131 `PAL8_LZ` frames from
something else on this machine - four workers share it. Nothing of mine was
running by then, and it is after the measurement.)

`cargo test -p screeny -p screeny-demos`: 105 tests, all pass (`pacing` 18.4 s,
first try, no flake). `cargo doc -p screeny-encode -p screeny --no-deps`:
silent. `cargo clippy -p screeny -p screeny-encode --all-targets`: clean (the
five warnings it does print are `screeny-demos`' pre-existing ones, card 125).
