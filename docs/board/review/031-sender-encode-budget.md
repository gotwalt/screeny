---
id: 031
title: Make the hybrid encoder's per-frame cost fit a 30 fps sender
type: build
hardware: no
depends: [005, 009]
owner: card-009 worker
branch: card/009-sender
---

## Goal

Establish, and if necessary fix, the wall-clock cost of the card 002 hybrid
encoder so a sender can sustain 30 fps with headroom for whatever is actually
generating the frames.

## Context

Card 002 recommends a sender that encodes each frame three ways (palette+LZ
ladder, dithered PAL5, BC1_DUAL), decodes each candidate, scores it by Oklab dE
through the panel model, and transmits the winner.

Measured in `lab/` on an M-series laptop: roughly **8 ms per frame,
single-threaded**, against a 33.3 ms budget. That fits, but:

- The lab encoder is written for clarity, not speed. The palette quantiser runs
  10 Lloyd iterations over the full colour histogram; the LZ match finder walks
  a 192-deep hash chain; scoring computes three cube roots per pixel per
  candidate.
- A Raspberry Pi or similar sender is several times slower, and cards 010/011
  want the sender to also be *generating* the frames.
- Nothing here has been profiled. 8 ms is a stopwatch number from the harness,
  not a benchmark.

## Deliverables

- A criterion (or equivalent) benchmark of the encoder in `crates/screeny`,
  broken down by stage: quantise, map, LZ, block fit, candidate scoring.
- Cheap wins applied where they do not cost quality. Candidates:
  - score candidates on a subsampled pixel set, or in a cheaper space than
    Oklab (the *ranking* may not need cube roots);
  - reuse the previous frame's palette as the Lloyd seed always, not only in
    the temporal-palette mode, and cut the iteration count;
  - skip candidates that cannot win — e.g. if the frame has under 256 distinct
    colours the palette ladder is lossless and no block mode can beat it;
  - cache the histogram between the ladder and the PAL5 candidate, which
    currently build it twice.
- A documented "fast" profile that trades a stated amount of dE for speed, if
  the full chooser turns out not to fit on the target sender.

## Acceptance

A measured frames-per-second figure for the encoder on the intended sender
hardware, with the quality cost of any shortcut quantified against the card 002
numbers using the same lab metrics.

## Log

### 2026-09-19 - measured, fixed, in review

Done as part of card 009; the encoder lives in `crates/screeny/src/encode/`.

**The 8 ms was a stopwatch number and it was measuring the tail.** Timed
properly - release, per frame, over the lab's own five clips, on an M-series
laptop with nothing else running - the lab's hybrid encoder is:

| | mean | p95 | worst frame |
|---|---|---|---|
| lab hybrid, **before** | **3.7 ms** | 7.1 ms | 7.5-11 ms |
| `crates/screeny`, `Full` | **1.45 ms** | 3.3 ms | 5.7-6.4 ms |
| `crates/screeny`, `Fast` | **0.78 ms** | 1.9 ms | 2.8-3.1 ms |

2.5x and 4.7x, and the figures repeat to within 3% across runs. On a busier
machine everything scales together - an earlier run measured the lab at 6.0 ms
and `Full` at 2.1 ms, the same 2.9x - so the ratio is the durable number and
the absolute one depends on what else the laptop is doing. `photo` is the
worst clip at 2.05 ms mean; `textui` is 0.05 ms because it never leaves the
lossless rung.

Against a 33.3 ms period that is **4.4% of a frame at the mean and 19% at the
worst frame**, leaving 32 ms for whatever is generating the frames. On a
sender several times slower - the Raspberry Pi the card worries about - `Full`
would be roughly 6 ms and `Fast` roughly 3 ms, both still comfortable.

**Where the time went.** `benches/encode` breaks it down by stage. Its corpus
is generated in-repo rather than borrowed from the frozen lab, so its absolute
numbers run lower than the table above (0.93 ms mean for `Full`); the
proportions are what this is for:

| stage | us/frame | |
|---|---|---|
| quantise | 586 | 63% |
| map | 132 | 14% |
| lz | 82 | 9% |
| block fit | 86 | 9% |
| histogram | 19 | 2% |
| scoring | 13 | 1% |
| decode | 5 | 1% |
| oklab | 3 | 0% |

Before any of the changes below, quantise was 85% of the same benchmark's
frame. The headline is that **candidate scoring was never the problem** - it
is 1% of the total and 13 microseconds a frame in absolute terms - so
the card's suspicion that the cube roots dominated was wrong. They were paid
somewhere else: inside the quantiser, once per distinct colour per Lloyd
iteration.

**What was done**, in order of what it bought:

1. **A k-d tree for "nearest palette entry"** (`encode/nn.rs`). Lloyd, palette
   costing and every mapping are all that one question, and all of them were
   linear scans: at `k = 256` over 2048 distinct colours that is half a
   million distance evaluations per iteration. The tree answers in `O(log k)`
   and answers *identically* - the leaf scan and the pruning test are arranged
   so ties go to the lowest index, exactly as a strict `<` does - so it cannot
   change an output byte. A unit test checks it against a linear scan on
   random palettes and on palettes full of deliberate duplicates. This alone
   took the benchmark's mean from 5.06 ms to 2.07 ms.
2. **One histogram per frame.** The lab rebuilt it up to six times, once in
   the ladder and once inside every `quant::build`, each through a
   `HashMap<[u8; 3], f32>`. It is now built once into reusable storage with an
   open-addressed table, and it also records each pixel's bin index, which
   turns every palette mapping from `O(NPIX * k)` into `O(bins * k)` plus one
   array read per pixel.
3. **Oklab per distinct colour, lazily.** A frame that takes a lossless rung
   never needs it at all.
4. **A lossless rung short-circuits the chooser.** If the ladder carried the
   frame exactly, nothing can beat zero error, so `PAL5` and `BC1_DUAL` are
   not built and nothing is scored. This is the card's "skip candidates that
   cannot win", generalised from "under 16 or 32 colours" to "any frame the
   ladder can carry losslessly", which is most pixel-authored content:
   `textui` never leaves it.
5. **Seeded palettes, and the fresh median cut only when it is needed.** The
   previous frame's palette is always the Lloyd seed now, at every ladder
   rung, not only in the temporal-palette mode. The lab's safety valve against
   a stale palette latching across a scene cut - compute a fresh median cut
   and keep the seeded one only if it is within 15% - is kept, but is no
   longer paid every frame: it runs when the seeded palette's cost drifts more
   than 25% from what the same palette size cost last frame, and once a second
   regardless.
6. **LZ scratch hoisted out of `deflate`.** The lab allocated and zeroed a
   256 KB hash head table on every call, up to six calls a frame. The table is
   also now 8192 buckets rather than 65536, which is still a load factor of a
   quarter for a 2 KB input.
7. **Median cut caches each box's split score.** It was recomputing every
   box's longest axis on every one of the `k - 1` splits.
8. **Scoring**: the source frame's emitted Oklab is computed once per frame
   rather than once per candidate, candidate colours go through a
   direct-mapped memo, and `cbrt` is a seed-and-two-Newton-steps version
   accurate to 2e-6 relative - four orders of magnitude below the differences
   being ranked, and pinned by a test against `f32::cbrt`.

The histogram is shared between the ladder and the `PAL5` candidate, which the
card listed as a cheap win, and it turned out to matter mostly because of what
it enabled in (2) rather than the duplicate build itself.

**Quality cost: none, on the whole corpus.** Mean panel-aware Oklab dE against
the frozen lab over its own five clips, same frames, same wire budget:

| clip | lab | `Full` | `Fast` |
|---|---|---|---|
| plasma | 11.48 | **10.50** | 12.31 |
| mandelbrot | 8.88 | 9.61 | 13.20 |
| textui | 0.00 | 0.00 | 0.00 |
| photo | 9.30 | 9.30 | 9.30 |
| darkfade | 1.05 | **0.98** | 1.14 |
| **all** | 6.14 | **6.08** | 7.19 |

`Full` is very slightly *better* than the lab overall, which was not the
intent. It comes from the seeding: a palette that tracks the previous frame's
flickers less, and that shows up in per-frame dE as well as in the temporal
metrics. The exception is `mandelbrot`, 8% worse, and that is the honest cost
of seeding - a fast zoom is the one case where the previous frame's palette is
genuinely stale. Tightening the reseed threshold recovers most of it (9.09 at
a 5% drift bound) but costs 40% more time and makes `plasma` worse, so the
looser setting was kept; reseeding *more* often than once a second is worse on
both axes.

**The documented `Fast` profile**, for when the full chooser does not fit:
LZ chains cut from 192 to 24, Lloyd iterations from 10 to 4 (2 when seeded),
the seeded palette trusted without a second opinion, and one pixel in four
scored on a lattice offset per row so it does not line up with the 8x8 Bayer
grid. **1.9x faster than `Full`, at +18% mean dE** (7.19 against 6.08), all of
which comes from the three many-colour clips. It is opt-in: `--fast` on the
CLI, `Profile::Fast` in the library. Given the headroom `Full` turned out to
have, nothing needs it on a Mac.

**Reproducing.** `cargo bench -p screeny` prints the table and the stage
breakdown against a corpus generated in `benches/encode/content.rs` - the same
five content classes as card 002, generated rather than checked in. No
criterion: the thing being measured takes milliseconds and the only comparison
that matters is against a 33.3 ms budget, so a median of repeated passes
answers it without the dependency tree. `screeny encode-stats --pattern NAME
--quality` gives a single number for one pattern. The lab-versus-screeny
harness is not in the repo - it depends on `lab/`, which is frozen - but it is
about 200 lines and the card 009 log describes what it does.

**Left undone:** all of this is measured on the card 002 synthetic clips. Card
032 is the real-content corpus, and if it changes the codec ranking it will
change these numbers too.
