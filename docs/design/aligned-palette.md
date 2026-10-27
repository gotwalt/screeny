# Aligned palette: a shared helper for hue-true panel colours

Status: **proposed** (card TBD, 2026-10-27). Design only; nothing here is built yet.
Source of truth for the panel facts: `generative-art-brief.md` section 2.1.1. If this
document disagrees with the brief, the brief wins.

Statuses as in the brief: **[measured]**, **[decided]**, **[provisional]**, plus
**[proposed]** for what this spec asks the owner to accept.

## 1. The problem

The panel has 64 hardware levels per channel in linear light, and very few of them near
black: the lowest sRGB codes that land on a level are 0, 34, 50, 62, 71, 80, 87, 94, 100
(brief 2.1.1, from `screeny_panel::aligned_levels`) **[measured]**. Anything between two
levels is made by the firmware alternating them over its 16-phase cycle. Since card 248
that alternation is fast, but near black it is still a shimmer up close, and alignment
is "the surer fix, and it is free once a shade is chosen".

Alignment is per channel, which is where hue goes wrong. `Panel::AlignedDark` (card 188)
rounds each channel below `DARK_ALIGN_LEVEL` to its own nearest level. That makes a pixel
steady, but three channels rounding independently can each go the other way, and a
muted colour - exactly the kind that lives near black - changes hue. Hearth's card 374
ramp is the worked example: its dark entries had green at a fraction of level 1, and
`aligned_dark` "snapped each channel its own way", so the cool reds banded and drifted
(comment on `hearth::DARK_RAMPS`) **[measured]**. `AlignedDark` is the backstop, not
the fix.

The fix the brief asks for is level *triples* chosen as a whole. Today patches get them
two ways:

- **By hand**: hearth (`DARK_RAMPS`, `SOOT_TRIPLES`), clocks and flavortime
  (`DARK_CHOICES` / `DARK_RAMPS`). Chosen by eye on the panel. Good, slow to make, and
  only practical for a few shades.
- **By search**: physarum's private `fn aligned(target, floor)`, which builds an
  84-entry palette (6 hues and a stone ramp, 12 lightness steps each) where every
  entry is an exact panel colour. This is the first patch to compute its triples.

**[decided]** by the owner: generalise the panel-specific part of physarum's approach
into `crates/art` so new patches get a steady, hue-true dark end for free. Aesthetic
choices (anchors, chroma shaping, convergence to stone, orientation-to-hue) stay in
patches. Hearth, clocks and flavortime are not migrated; their ramps were chosen by eye
and a patch's default may not move its own pixels (brief 2.1.1).

## 2. What physarum does today **[measured]**

`physarum::aligned(target: Rgb, floor: f32) -> (Rgb, f32)`:

1. For each channel, find `at`, the highest level whose linear light is `<=` the
   target's, and take levels `at-1, at, at+1, at+2`, clamped to 0..63. That gives a
   4x4x4 neighbourhood: 64 candidate triples.
2. Skip `(0,0,0)`. Black is the caller's (physarum's palette index 0).
3. Skip any candidate whose OKLab L is not above `floor + 0.004`.
4. Score the rest with `dL^2 + 1.5 * (da^2 + db^2)` in OKLab: hue and chroma weigh more
   than lightness, because "a step that is a shade off is fine, one that changes colour
   is not". Keep the lowest. Ties go to the first found (`<`, ascending r, g, b).
5. If nothing passes, return the triple `at+1` on every channel. This is the fallback.

`Physarum::palette` calls this once per step, carrying `floor` forward as the previous
entry's L, so each ramp rises. The result is cached per value of `mute`.

I dumped the palette in a throwaway test (not committed), on x86-64, release build:

- With `mute` at 0, 0.01, 0.5, 1, 1.37 and 2, all 84 entries rise, and **the fallback
  never fires**.
- Every ramp starts at level-1 grey `(1,1,1)` (sRGB 34 34 34, L 0.252). The next steps are
  `(2,1,1)` or `(1,1,2)`, and the hue only takes hold from about step 3. That is the
  "converging to a shared grey" look, and it comes from the bottom of the panel, not
  from anything the patch asked for.
- Above the dark end, "exact" means each channel is `level_code(level)`. That code can
  sit up to **7/16 of a level** past the level (levels 47, 55, 56 and 62; up to 2/16 below
  level 16). The brief says that is a few percent of the light up there and invisible.
  It still matters for tests (section 6).
- Cost: **3.4-4.5 µs per call, about 0.7 ms for the 84-entry palette** in release. A
  debug build is several times slower.
- Edge behaviour: a black target with floor 0 returns `(0,0,1)` (dim blue, L 0.114), the
  darkest non-black triple. A near-black *grey* (linear 0.002) returns `(1,0,0)`, a red:
  below level-1 grey's lightness there is no neutral triple, and the metric prefers a
  close L with the wrong hue to a grey that is far too bright. A floor of 0.999 returns
  white, which does *not* rise above the floor. The fallback's "keep rising" promise
  does not hold at the ceiling.

## 3. Proposed API **[proposed]**

A new file `crates/art/src/panel/aligned.rs`, declared as `mod aligned;` from
`crates/art/src/panel.rs` and re-exported there, so callers write
`crate::panel::{nearest_aligned, aligned_ramp, Aligned}`.

**Why there.** `panel.rs` is "the art side's reading of `screeny_panel`" and already owns
`level_code` and `level_triple`, the only way this crate turns a level into a colour.
The new helper is another reading of the same table, so it belongs in that namespace.
A separate file keeps OKLab search code out of the `Panel` quantiser. It also does not
belong in `crates/panel` (`screeny_panel`): that crate models what the panel *does* to a
colour, and choosing which colour to ask for is art-side design.

```rust
/// A colour the panel shows the same way on every refresh: each channel is
/// `level_code(levels[i])`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aligned {
    pub rgb: Rgb,          // level_triple(levels), linear light
    pub levels: [u32; 3],
    pub l: f32,            // OKLab lightness of `rgb`
    pub clipped: bool,     // true when nothing above `floor` was reachable
}

/// Minimum OKLab L step between two entries of a ramp.
pub const ALIGNED_RISE: f32 = 0.004;
/// Weight of OKLab a/b (hue and chroma) against L in the search.
pub const ALIGNED_HUE_WEIGHT: f32 = 1.5;

/// The non-black level triple nearest `target` in OKLab (weighted as above)
/// whose L exceeds `floor + ALIGNED_RISE`. `floor = 0.0` is "no floor".
pub fn nearest_aligned(target: Rgb, floor: f32) -> Aligned;

/// One aligned entry per target, each strictly lighter than the last
/// (unless `clipped`). Targets are usually made with `color::oklch`.
pub fn aligned_ramp(targets: impl IntoIterator<Item = Rgb>) -> Vec<Aligned>;
```

Also in `panel.rs`: `pub fn level_linear(level: u32) -> f32`, read from `level_code`
through one `OnceLock` LUT. This replaces physarum's private `LIN` table, which is a
second copy of `level_code` decoded, with a literal `64` in it.

### Search rules

- **Metric**: OKLab, `dL^2 + 1.5 * (da^2 + db^2)`. These are physarum's numbers, kept as
  they are, because byte identity depends on them (section 5). They are named constants
  so an opt-in variant can change them later. They are not parameters.
- **Neighbourhood**: 4x4x4 levels per channel, as in section 2, with the bounds from
  `NATIVE_LEVELS`, not a literal 63.
- **Excluded**: `(0,0,0)`. A helper that can return black would let a ramp's bottom step
  disappear. Black stays the caller's to place.
- **Floor**: strict `L > floor + ALIGNED_RISE`. `aligned_ramp` starts at floor 0.0 and
  carries each entry's L forward.
- **Nothing reachable above the floor** (only seen at the ceiling): widen the window
  upward, one level per channel at a time, until a candidate passes. If even white
  `(63,63,63)` does not pass, return white with `clipped: true`. Physarum's fallback never
  fires, so its bytes are not affected. The difference from today is that
  "rising" is guaranteed or flagged, instead of quietly failing at the top.
- **Bottom**: the darkest neutral is level-1 grey, L 0.252. A target darker than that
  comes back as whichever level-1 primary or pair is closest, possibly in a hue the
  target never had (section 2). This is documented, not hidden. A patch that wants a
  neutral bottom starts its ramp at L >= 0.25, or puts black under it the way physarum
  does with `FLOOR`. See open question 2.
- **Top**: white is reachable and nothing is clamped above it. Entries above
  `DARK_ALIGN_LEVEL` are aligned too, which costs nothing, keeps a ramp in one colour
  all the way up, and is what physarum does.
- **Determinism**: pure f32 arithmetic in a fixed order, no RNG and no state beyond
  the `OnceLock` LUT. Same inputs give the same triple on a given build.
  **[provisional]** `cbrt` comes from the platform's libm, so a near-tie could in
  principle flip between x86-64 and aarch64 (the HA add-on). The golden test (section 6)
  runs where CI runs. If a flip ever appears, add a tie margin rather than loosen the
  test.

### Cost and caching

About 4 µs per entry **[measured]**. That is fine for a palette built when a parameter
changes, and wasteful every frame. It is **not for per-pixel use**: 2048 pixels would
cost about 8 ms. The rule for callers: build the palette once per change of whatever
shapes it, and cache it, the way physarum caches per `mute`. The helper does not cache
for you, because only the patch knows what its targets depend on. If a patch needs a
per-pixel aligned colour, it should index into a cached ramp, not search.

## 4. How it relates to what exists

| Piece | Role after this change |
|---|---|
| `screeny_panel::aligned_levels`, `duty_16ths`, `nearest_level` | Unchanged. Still the only source of levels. |
| `panel::level_code`, `level_triple` | Unchanged. The helper builds every candidate with `level_triple` and so adds no new table. |
| `panel::level_linear` (new) | The decoded LUT the search needs, one copy, read from `level_code`. |
| `Panel::AlignedDark`, `DARK_ALIGN_LEVEL` | Unchanged, and still the backstop for anything a patch did not align itself. Helper output is a **fixed point** of it: a code that is `level_code(l)` re-rounds to `l` (offset <= 2/16 below level 16), so `AlignedDark` leaves aligned entries alone. Section 6 tests this. |
| Hand-picked ramps (hearth, clocks, flavortime) | Untouched. `Aligned::levels` lets a computed triple be printed and pasted into such a table, so the two styles meet. |

## 5. Migration

**Physarum**: switch to the helper with byte-identical palette output.

1. **Before the switch**, in its own commit, factor `Physarum::palette`'s body into a
   pure `fn palette_for(mute: f32) -> Vec<Rgb>` (no `Ctx` and no GPU, so a test can call
   it), and add the golden test in section 6, passing against the current private
   `aligned`.
2. Replace the private `aligned` and `LIN` with `aligned_ramp` over the same targets
   (`oklch(l, c, hue)` from the existing closure). Delete the private function.
3. The golden test must pass **unchanged**. As a manual cross-check, run anything that
   renders physarum with `PHYSARUM_PALETTE=1` before and after, and `diff` the stderr
   `physarum palette` lines for `mute` 1. Keep the env dump. It is the patch author's
   window onto the palette.

Nothing about what physarum looks like changes. That is the migration's only test.

**Hearth, clocks, flavortime**: not touched. Their defaults are their own pixels. The
future path, if wanted, is a **new named choice** in that patch's existing `dark`
parameter (for example `"computed"`), built with `aligned_ramp` from the targets the
ramp was originally derived from, judged on the panel against the hand-picked entries,
and never made the default without the owner looking at it.

## 6. Tests

Few and fast. Pure functions, no simulation and no GPU. Together they should take
milliseconds even in debug.

In `panel/aligned.rs`:

1. **Every entry is three level codes.** Run a small grid (OKLCH L 0.2-0.9 in 8 steps, 12
   hues, chroma 0 / 0.05 / 0.12, about 300 calls) through `nearest_aligned` and check
   `rgb.to_srgb8() == levels.map(level_code)`. Also check that
   `Panel::AlignedDark.snap8(rgb, ±0.49)` returns the same bytes, which is the
   fixed-point claim. Do not assert a zero `offset_16ths`: above the dark end it is up
   to 7.
2. **Ramps rise strictly.** Run a neutral ramp, a muted warm ramp and a saturated blue
   ramp (12 steps, L 0.2 to 0.9) through `aligned_ramp`. Check that each L exceeds the
   previous by more than `ALIGNED_RISE` and that nothing is `clipped`.
3. **The ceiling is flagged, not broken.** `nearest_aligned(white, 0.999)` returns white
   with `clipped`, without panicking.
4. **Muted hue holds.** For muted targets (chroma 0.05, L >= 0.4, 12 hues), the result's
   OKLab hue is within a bound of the target's. Set the bound from measurement when
   implementing; on physarum's own ramps no entry with chroma above 0.03 is more than 20°
   off at `mute` 1. This test is the reason the helper exists, so it should fail if
   someone swaps the search for per-channel rounding.

In physarum:

5. **Palette unchanged.** `palette_for(mute)` for `mute` in {0, 1, 2}: compare a 64-bit
   FNV-1a of the sRGB bytes against a pinned value. On mismatch, print the
   `PHYSARUM_PALETTE`-style dump so the diff is readable. A checksum instead of 252
   literal triples is open question 4.

## 7. Proposed addition to brief 2.1.1 (draft, not yet applied)

To go after the paragraph on `level_triple` and the clocks example:

> A patch that wants *many* held shades - a ramp per hue, a palette that is computed
> rather than picked - does not have to choose triples by hand.
> `screeny_art::panel::aligned_ramp(targets)` takes target colours (usually
> `color::oklch` stops, darkest first) and returns one level triple per target, each the
> nearest in OKLab with hue weighted over lightness, each strictly lighter than the last.
> `nearest_aligned(target, floor)` is the single-colour version. Every entry is exact,
> so it cannot shimmer, and the three channels are chosen together, so a muted hue
> survives where `aligned_dark`'s per-channel rounding would let it drift. The bottom of
> the panel still rules: the darkest neutral is level-1 grey (sRGB 34), and below it the
> only non-black triples are level-1 primaries, so start a hue-true ramp at OKLab L 0.25
> or put black under it. Build it once per parameter change and cache it; it is too
> slow per pixel. `crates/art/src/patches/physarum` is the worked example. The
> hand-picked ramps in hearth and clocks stay as they are; the helper is for new
> patches, or for a new opt-in choice in an old one.

## 8. Alternatives considered

- **Per-channel rounding (`AlignedDark`).** Steady, but no joint view of the colour, so
  muted hues drift (section 1). Kept as the backstop.
- **Duty-space metric** (distance in sixteenths of a level, or linear light). This is
  what the panel actually does, but it is not perceptual: it overweights bright channels
  and cannot tell "a shade off" from "a different colour". The brief asks for ramps that
  "read as the same material", which is a hue question, so OKLab with hue weighted wins.
- **A larger neighbourhood** (6x6x6 is 3.4x the cost). Not needed so far: the fallback
  never fired. It might matter for saturated targets at the bright end, where the
  hue-correct triple can sit more than two levels from the target on some channel.
  Revisit if test 4 needs it. Widening only on the fallback path (section 3) already
  covers the ceiling.
- **Aligning only below `DARK_ALIGN_LEVEL`** and passing brighter entries through.
  Cheaper in principle, but it breaks byte identity with physarum, and a ramp that
  changes method halfway up risks a visible seam. Rejected for now.
- **A precomputed triple-to-OKLab table** (64^3 entries, about 3 MB). Rejected: caching
  palettes per parameter is enough.
- **Spatial dithering between two aligned shades** (brief: "mix them in space, not in
  time"). This is complementary, not an alternative. A later
  `aligned_pair(target) -> (Aligned, Aligned, f32)` could hand a patch two neighbouring
  exact shades and a mix for an ordered pattern (`crate::dither`). Out of scope here.

## 9. Open questions for the owner

1. **The ceiling fallback.** The proposal is widen-then-flag (`clipped`). The other
   option is to keep physarum's `at+1` rule as it is. Neither changes physarum's bytes.
2. **Below level-1 grey.** Keep today's "nearest, even if the hue jumps to a level-1
   primary", or add an opt-in that never leaves the target's hue family (for example,
   fall back to grey or return black)?
3. **Return type.** Is `Aligned { rgb, levels, l, clipped }` right, or should the public
   functions return plain `Rgb` and leave the rest to an inner function? `levels` is what
   makes "print it and pin it by hand" possible.
4. **Golden form for physarum.** A checksum (small, opaque, dumps on failure) or an
   explicit table of 84 triples for `mute` 1 (readable, long)?
5. **Card number** for this work, and whether the brief addition (section 7) lands in
   the same change as the helper or after physarum has run on it for a while.
