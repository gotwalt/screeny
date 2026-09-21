# Vendored crates

## `hub75-framebuffer` 0.12.0

Vendored from crates.io, unmodified except for the changes listed here, and
wired in with `[patch.crates-io]` in `firmware/Cargo.toml` so that
`esp-hub75` 0.17.0 uses the same copy (its public types *are* these types, so
a second dependency would not do).

Upstream: <https://github.com/liebman/hub75-framebuffer>.

### Why

`esp-hub75` / `hub75-framebuffer` have no brightness control at all. Card 001
concluded that our only lever was scaling pixel values, which at Tidbyt's own
default brightness throws away about three of our six bit-planes — see card
020.

That conclusion is wrong, and the reason is visible in
`src/bitplane/plain/mod.rs`: the output-enable line is **bit 8 of every entry
in the framebuffer**. `make_data_template()` lights the slots
`TRAIL_BLANK_DELAY .. COLS - LEAD_BLANK_DELAY - 1` of each 64-slot scan row
and blanks the rest, at build time. Nothing except that function ever writes
bit 8 — `set_pixel` masks bits 9..14 and `erase` masks bits 9..14 — so the
window can be narrowed at run time, per buffer, without disturbing colour
data, the row address or the latch.

Narrowing that window is exactly the mechanism Tidbyt's firmware calls
`setBrightness8()`: the panel is lit for `n/64` of each row period. It costs
no colour depth (all planes keep their weights) and no refresh rate (the DMA
stream is the same length). It is the answer to card 020.

### The changes

All of them are in `src/bitplane/plain/frame.rs`, on `DmaFrameBuffer` only (the
layout we use), and all are additive:

- `pub const OE_SLOTS` — the widest achievable output-enable window, in
  pixel-clock slots per scan row. With `trail-blank-8` and no lead blank on a
  64-column panel this is 55, i.e. full brightness is 86% duty.
- `pub const OE_DEFAULT_START` — the slot the compile-time `trail-blank-N`
  feature starts that window at, so a caller can move the window and come back
  to the default without depending on this crate for the constant.
- `pub fn set_oe_slots(&mut self, lit: usize)` — rewrites bit 8 of every entry
  so the panel is lit for `lit` of those slots. Clamped to `OE_SLOTS`. This is
  the brightness control.
- `pub fn set_oe_window(&mut self, start: usize, lit: usize)` — the same, but
  also moves where in the scan row the lit window begins. This is the
  anti-ghosting control the `trail-blank-N` features set at build time, made
  runtime-adjustable so it can be swept against a camera rather than by
  rebuilding. Card 007 did sweep it and found this panel does not ghost at any
  setting; it stays because the sweep should be repeatable on the next panel.
- `pub fn write_row(&mut self, y, &[[u8; 3]; COLS])` — writes one whole display
  row of already-quantised colour, hoisting the `planes[p].rows[r]` lookup out
  of the per-pixel loop and always writing (so a full-frame conversion needs no
  `erase()` pass). Measured 2.3x faster than the equivalent `set_pixel` loop on
  this chip; see card 007's log.

Nothing in `src/lib.rs` is touched. An earlier revision of this patch made
`LEAD_BLANK_DELAY`, `TRAIL_BLANK_DELAY` and `INTER_ROW_BLANK` `pub` so the
firmware could read the trail blank; that was reverted in favour of
`OE_DEFAULT_START`, which needs no second dependency on this crate and does not
trip its own `#![warn(missing_docs)]`.

Nothing was removed or altered, so upstream behaviour for anyone not calling
the new functions is byte-identical. The `benches/` directory was dropped
because it pulls in `criterion` and we never run it on this target.

### The sub-level plane that is not here (card 248)

Card 248 asked for planes *below* plane 0, shown once per refresh with a
narrowed output-enable window: a real half-level at the full refresh rate,
instead of the temporal dither making half-levels by skipping whole refreshes.
The mechanism fits this crate cleanly and **is not implemented**, for a reason
that has nothing to do with this crate. In case somebody comes back to it:

- `bcm_sequence` streams contiguous plane *suffixes* and a plane's coverage is
  the sum of the reps of every segment starting at or before it. Put `SUB`
  sub-planes at the **front** of `planes` and the wanted coverage
  (`1, …, 1, 1, 2, 4, …`) has first differences `r[0] = 1`, `r[1..=SUB] = 0`,
  `r[SUB+1] = 1`, `r[SUB+k] = 2^(k-1)`. Omit the zero-rep segments and the
  sequence is `PLANES - SUB` entries - six for `SUB = 2, PLANES = 8`, the same
  count as today - with only the first segment spanning the sub-planes, so each
  is displayed exactly once per refresh. `SUB = 0` reproduces today's sequence,
  so the change is additive.
- The only structural obstacle is that `bcm_segment_ptr(i)` assumes segment
  index *is* plane index. It would need a `SEG_START` table beside
  `BCM_SEQUENCE`.
- `write_row` needs no new signature: the value becomes the level in
  quarter-levels (`0..=252`, still a `u8`) and plane `k` still carries bit `k`.
- `set_oe_slots` would gain a per-plane width (`lit >> 1`, `lit >> 2`; floor,
  because floor is the only rounding that keeps `half + quarter < lit` at every
  brightness step) and would stay the one place bit 8 is written.

What stopped it: **RAM**. A plane is 2,052 bytes here and there are two
framebuffers, so a sub-plane costs 4,104 bytes of `.data`, and on this chip
`.data` comes straight off core 0's stack. `tools/fw-size.sh` reported `.stack`
at 25,792 against its own 24,576 floor - 1,216 bytes of headroom. Card 248's
log has the full design and the arithmetic; card 249 is the lever that would
pay for it.

### Upstreaming

`set_oe_slots` is worth offering upstream — every USB-powered panel wants it,
and it is ~30 lines. It would want the same treatment for the `latched` and
row-major layouts, plus a `Hub75Config::brightness` convenience, which is
more work than this card can carry. Card 060 records that.
