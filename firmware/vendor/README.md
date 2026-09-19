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

1. `src/lib.rs`: `LEAD_BLANK_DELAY`, `TRAIL_BLANK_DELAY` and
   `INTER_ROW_BLANK` changed from `pub(crate)` to `pub`. No behaviour change;
   the firmware needs them to size its brightness scale.

2. `src/bitplane/plain/frame.rs`, on `DmaFrameBuffer` only (the layout we
   use), all additive:

   - `pub const OE_SLOTS` — the widest achievable output-enable window, in
     pixel-clock slots per scan row. With `trail-blank-8` and no lead blank
     on a 64-column panel this is 55, i.e. full brightness is 86% duty.
   - `pub fn set_oe_slots(&mut self, lit: usize)` — rewrites bit 8 of every
     entry so the panel is lit for `lit` of those slots. Clamped to
     `OE_SLOTS`. This is the brightness control.
   - `pub fn write_row(&mut self, y, &[[u8; 3]; COLS])` — writes one whole
     display row of already-quantised colour, hoisting the
     `planes[p].rows[r]` lookup out of the per-pixel loop and always writing
     (so a full-frame conversion needs no `erase()` pass). Measured 2.3x
     faster than the equivalent `set_pixel` loop on this chip; see card 007's
     log.

Nothing was removed or altered, so upstream behaviour for anyone not calling
the new functions is byte-identical. The `benches/` directory was dropped
because it pulls in `criterion` and we never run it on this target.

### Upstreaming

`set_oe_slots` is worth offering upstream — every USB-powered panel wants it,
and it is ~30 lines. It would want the same treatment for the `latched` and
row-major layouts, plus a `Hub75Config::brightness` convenience, which is
more work than this card can carry. Card 060 records that.
