# 002 - Frame encoding: best picture in one 1472-byte datagram

Everything below comes out of `lab/`. `cargo run --release` there reproduces
every number and every PNG; `cargo test --release` checks the encoders and
decoders agree. Card 002.

---

## Conclusions

**Ship four decoders and let the sender choose per frame.** The sender encodes
each frame three ways, decodes each candidate, scores it, and sends the winner;
the mode byte already on the wire tells the firmware which decoder to run, so
this costs the device nothing. That "hybrid" beat every single fixed codec on
every content class we tested, at **mean panel-aware dE 6.5 against the best
fixed-rate codec's 9.5**, and it is the only approach that is simultaneously
*lossless on text/UI* and *good on photographs*.

| | |
|---|---|
| Pixel budget | **1464 B** (1472 UDP payload - 8 B header), mode byte included |
| Worst observed frame | **1464 B**. Nothing ever exceeds budget: the variable-rate path walks a quality ladder down to a 1377 B fixed-rate floor |
| Decoders to implement | `PAL4_LZ`, `PAL8_LZ`, `PAL5`, `BC1_DUAL` (+ `SOLID` as a trivial floor) |
| Worst-case decode | **~131 k Xtensa instructions ≈ 0.55-0.82 ms**, 1.6-2.5% of a 33.3 ms frame |
| Decoder RAM | zero beyond the frame buffer. No scratch, no allocation, no tables over 8 entries |
| Loss behaviour | every mode is fully self-contained. A lost datagram costs exactly one frame |

### The four modes

All payloads begin with a **mode byte**; sizes below include it. Multi-byte
fields are little-endian. Index planes are raster order, MSB-first within a
byte. `x4`/`x5`/`x6` mean bit-replicating expansion to 8 bits.

**`0x10 PAL8_LZ` — adaptive palette, LZ-compressed indices. Variable rate.**
```
[0x10][n-1 : 1][palette : n x RGB888][LZ stream -> 2048 index bytes]
```
The workhorse. Lossless whenever the frame has ≤256 distinct colours and the
compressed indices fit; otherwise the palette shrinks (256 → 128 → 64 → 32)
until it does. Decodes the LZ stream into the front of the frame buffer and
then expands indices to pixels in place, walking backwards — **no scratch RAM**.

**`0x11 PAL4_LZ` — 16-colour palette, LZ-compressed nibble plane. Variable rate.**
```
[0x11][palette : 16 x RGB888][LZ stream -> 1024 nibble bytes]
```
The same idea at half the index plane. This is what text/UI frames land on:
**389 B mean, 401 B max, bit-exact**, 3.7x under budget.

**`0x28 BC1_DUAL` — 4x4 blocks, per-block endpoint/gradation trade. 1297 B fixed.**
```
[0x28][flags : 16 B, one bit per block][128 blocks x 10 B]

flag 0:  [e0 : RGB565][e1 : RGB565][idx : 3 bitplanes x 2 B]   8 levels
flag 1:  [e0 : RGB888][e1 : RGB888][idx : 16 x 2 bits, 4 B]    4 levels
```
New; not a standard format. Measurement said a block can be short of two
different things — *endpoint precision* (dark, smooth blocks, where RGB565's
5-bit steps are coarse relative to what the panel resolves) and *gradation*
(bright ramps, where four levels band). One flag bit per block buys whichever
that block needs, and both alternatives happen to cost exactly 10 bytes.
It **matches or beats both of its parents on all five clips** (see below).

Levels are `lerp(e0, e1, W[k])` with `W4 = [0,85,171,256]`,
`W8 = [0,37,73,110,146,183,219,256]` and `lerp(a,b,w) = (a*(256-w) + b*w + 128) >> 8`.
Those tables are exact complements (`W[k] + W[n-1-k] == 256`), so BC1's 1/3 and
2/3 points are reproduced to within one code value with a multiply and a shift
instead of a divide. The S3TC spec explicitly permits this slack, and we own
both ends anyway. **Endpoints are stored and interpolated in sRGB (gamma)
space**, not linear: on a linear-light panel the perceptually even ramp between
two colours is the one that is even in sRGB, so this is both the cheap option
and the correct one.

**`0x02 PAL5` — 32-colour palette, 5 bpp. 1377 B fixed. The floor.**
```
[0x02][palette : 32 x RGB888][low nibbles : 1024 B][bit 4 plane : 256 B]
```
Plane-split rather than a 5-bit bitstream so decode is `idx = nib | (bit << 4)`
— no bit reader, no unaligned reads, one branchless expression per pixel.
This rung always fits, which is what makes the variable-rate path safe.

**`0x7f SOLID`** — `[0x7f][RGB888]`, 4 bytes. Not used in practice; it exists so
"cannot fit" is structurally impossible.

### Sender algorithm

```
candidates = [ palette_ladder(frame, budget),      # PAL4_LZ / PAL8_LZ / PAL5
               pal5_dithered(frame),               # PAL5, palette seeded from t-1
               bc1_dual(frame) ]                   # BC1_DUAL
score each by decoding it and measuring mean Oklab dE through the panel model
give the previous frame's mode an 8% handicap advantage (hysteresis)
send the winner
```

Two details that the measurements forced:

* **Score against a *better* panel than we have.** Selection uses the 6-bit
  panel *with* device-side temporal dithering, not today's plain 6-bit panel.
  Codecs with RGB444 endpoints (`blk42`, `cc4`) look competitive today only
  because the panel is too coarse to show their error; under a dithered panel
  they fall apart (`cc4` on the dark clip: dE 10.6 → 35.0). Selecting against
  the better panel means the picture cannot get *worse* when the firmware gets
  better. It is also why `blk42` and `cc4` are not in the shipping set even
  though both beat `bc1-dual` on the panel we have *today at today's
  brightness* (3-bit column: 9.26 and 10.40 against 10.56).
* **Hysteresis.** Switching mode changes the *character* of the error (block
  edges vs. dither noise) and the eye notices that even at equal magnitude.

Modes actually selected, by clip: `plasma` PAL8_LZ 98%; `mandel` PAL8_LZ 100%;
`textui` PAL4_LZ 100%; `photo` BC1_DUAL 100%; `darkfade` PAL8_LZ 65% /
BC1_DUAL 28% / PAL5 6%.

---

## How this was measured

### The panel model — and why it changes the answer

Card 001 measured the Rust `esp-hub75` driver on this panel at the Tidbyt's
10 MHz pixel clock: **6 bits/channel gives ~154 Hz refresh**, 7 bits gives
~76 Hz (visibly flickering), 8 bits is unusable. So the output stage is ~6-bit
**linear light**, and an LED's output is proportional to BCM duty, which means
the firmware's sRGB8 → duty table *is* the gamma correction.

Every perceptual metric therefore pushes both source and decode through
`panel::emit` before comparing: sRGB EOTF, then quantise to the panel's steps.
Error the panel cannot show does not count. Scoring against an 8-bit sRGB
display would over-reward codecs that spend bytes on precision.

| panel | bitplanes | refreshes dithered across | distinct output levels from 256 sRGB codes | sRGB codes that emit nothing |
|---|---|---|---|---|
| dimmed today (30/255 brightness) | 3 | 1 | 8 | 76 (29.7%) |
| | 4 | 1 | 16 | 52 (20.3%) |
| | 5 | 1 | 32 | 35 (13.7%) |
| **card 001 measured** | **6** | **1** | **64** | **22 (8.6%)** |
| | 7 | 1 | 126 | 13 (5.1%) |
| | 8 | 1 | 183 | 7 (2.7%) |
| 6-bit + 5x device temporal dither | 6 | 5 | 195 | 6 (2.3%) |
| 6-bit + 10x | 6 | 10 | 224 | 3 (1.2%) |

Read the last two rows: **six bitplanes plus temporal dithering across the five
panel refreshes that fit inside one 30 fps frame is worth more than eight
bitplanes with none.** 195 distinct levels vs 183. That is a firmware change,
not a codec change, and it is the single highest-leverage item this card found
— see card 030.

One more number from that direction, worth stating plainly: under the **3-bit
dimmed** panel (what you get today at a Tidbyt-like 30/255 brightness, because
the driver has no OE-duty control), the entire `darkfade` clip scores dE 0.00
for *every codec in the study*. Not "close" — identical. At three linear bits
every pixel in that clip emits zero duty, so the encoding is irrelevant because
the panel shows black either way. Choosing a codec to suit that panel would be
choosing a codec for a display that is not working. Fix brightness first.

### Metrics

* **dE** — Euclidean Oklab distance between panel-emitted source and panel-emitted
  decode, mean over pixels and frames, x1000. The headline number.
* **dE blurred** — the same after a separable `[1 2 1]/4` low-pass of *emitted
  light*. A "viewed from across the room" check that treats high-frequency error
  (dither) more kindly than low-frequency error (banding, block flatness).
  Deliberately weak: at a normal viewing distance a 1.9 mm pixel still subtends
  ~6 arcmin, which the eye resolves easily, so a strong blur would be flattery.
* **SSIM** — 7x7 windows on gamma-space Rec.709 luma. Structure, which dE misses.
* **px exact** — percentage of pixels reproduced bit-exactly. The metric that
  matters for content class (b).
* **flicker** — mean frame-to-frame change in emitted Oklab L, *minus* the same
  quantity for the source. Positive means the codec adds shimmer the content
  does not have.
* **dE t-avg4** — error of the 4-frame average of emitted light, roughly what
  the eye integrates. Temporal dithering trades `flicker` against this.

### Content

Rendered at 4x and box-filtered down *in linear light*, except the text/UI mock
which is authored at 1x because class (b) content is pixel-exact by nature.
Rendering visualiser content directly at 64x32 would produce artificially
palette-friendly frames and flatter the palette codecs.

| clip | frames | distinct colours/frame | what it probes |
|---|---|---|---|
| `plasma` | 60 | 999 | smooth full-frame motion, fully saturated hues, no hard edges |
| `mandel` | 60 | 725 | fine high-contrast detail plus saturated colour cycling |
| `textui` | 60 | 7 | pixel-authored UI, hard edges, mostly static, scrolling marquee |
| `photo` | 60 | 1653 | broadband detail, texture, sun highlight, slow pan |
| `darkfade` | 60 | 90 | near-black gradient with a few bright stars — the dark-end probe |

---

## Results

Mean of per-clip means across all five clips. dE columns are x1000; lower is
better everywhere except SSIM. `flicker` is signed: negative means the codec
is *steadier* than the source (it is smoothing motion away).

| codec | bytes mean | bytes max | **dE(6-bit)** | dE p95 | dE blurred | dE(6-bit +tdith) | dE(3-bit) | dE(8-bit) | SSIM | flicker | dE t-avg4 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `hybrid` **(recommended)** | 1029 | 1464 | **6.53** | 21.00 | 3.33 | 6.14 | 7.15 | 6.24 | 0.9862 | +0.59 | 4.40 |
| `pal-lz` | 984 | 1464 | **7.15** | 20.14 | 3.65 | 6.76 | 8.01 | 6.80 | 0.9846 | +1.19 | 4.60 |
| `pal5-adapt` | 1377 | 1377 | **9.52** | 25.11 | 5.14 | 9.18 | 11.69 | 9.19 | 0.9767 | +1.53 | 6.42 |
| `pal5-adapt-ord-tp` | 1377 | 1377 | **10.70** | 29.78 | 5.41 | 10.43 | 12.45 | 10.51 | 0.9666 | +1.11 | 7.25 |
| `pal5-adapt-tdith` | 1377 | 1377 | **10.72** | 29.81 | 5.43 | 10.44 | 12.51 | 10.53 | 0.9665 | +2.68 | 6.51 |
| `pal5-adapt-ord` | 1377 | 1377 | **10.77** | 29.93 | 5.50 | 10.46 | 12.97 | 10.56 | 0.9695 | +2.41 | 7.10 |
| `bc1-dual` | 1297 | 1297 | **11.01** | 58.18 | 6.12 | 10.61 | 10.56 | 10.64 | 0.9821 | -0.22 | 9.03 |
| `blk42` | 1281 | 1281 | **11.30** | 61.18 | 6.90 | *16.47* | 9.26 | 15.53 | 0.9468 | -0.03 | 8.87 |
| `bc1-i3` | 1281 | 1281 | **11.63** | 58.19 | 6.62 | 13.36 | 10.67 | 13.23 | 0.9782 | -0.23 | 9.64 |
| `pal5-adapt-fs` | 1377 | 1377 | **12.44** | 32.80 | 5.57 | 12.30 | 14.94 | 12.46 | 0.9619 | +3.24 | 8.30 |
| `cc4` | 1281 | 1281 | **12.82** | 63.36 | 7.48 | *18.01* | 10.40 | 16.63 | 0.9439 | +0.32 | 9.90 |
| `bc1-e888` | 1281 | 1281 | **13.12** | 63.76 | 6.70 | 13.40 | 12.81 | 13.46 | 0.9776 | -0.07 | 10.41 |
| `cc2-42` | 1281 | 1281 | **13.22** | 52.92 | 6.23 | 15.84 | 13.29 | 16.77 | 0.9708 | -0.24 | 10.63 |
| `bc1` | 1025 | 1025 | **14.09** | 65.47 | 7.53 | 16.60 | 13.17 | 17.23 | 0.9729 | +0.10 | 11.18 |
| `pal4-adapt` | 1073 | 1073 | **15.10** | 37.13 | 8.85 | 15.15 | 18.95 | 15.65 | 0.9473 | +2.15 | 10.45 |
| `blk84-i3` | 1025 | 1025 | **16.73** | 79.57 | 10.95 | 18.09 | 15.89 | 17.83 | 0.9562 | -0.26 | 14.27 |
| `pal4-adapt-ord` | 1073 | 1073 | **17.04** | 45.11 | 9.24 | 17.06 | 20.95 | 17.53 | 0.9348 | +3.53 | 11.61 |
| `cc2-44` | 1025 | 1025 | **18.14** | 56.72 | 8.84 | 18.94 | 19.47 | 19.12 | 0.9588 | -0.59 | 14.88 |
| `ycocg-410` | 1441 | 1441 | **36.07** | 138.41 | 25.42 | 39.36 | 27.85 | 40.15 | 0.9355 | -0.07 | 34.09 |
| `ycocg-420` | 1409 | 1409 | **41.00** | 128.41 | 27.99 | 43.86 | 28.93 | 42.49 | 0.8687 | +0.93 | 38.10 |
| `pal5-fixed` | 1377 | 1377 | **64.29** | 176.39 | 40.25 | 65.32 | 43.72 | 64.34 | 0.7335 | +4.26 | 55.61 |
| `pal4-fixed` | 1073 | 1073 | **80.51** | 185.38 | 46.51 | 94.55 | 68.51 | 93.94 | 0.6135 | +1.94 | 70.80 |

No codec ever exceeded the budget on any frame of any clip. Per-clip tables,
with PSNR, pixel-exactness and per-frame mode mixes, are in `lab/out/results.md`.

### The two clips that decide it

**`textui`** (pixel-authored UI, 7 colours) — this is where block codecs lose
outright. Nothing block-based can be pixel-exact on glyph edges:

| codec | bytes | dE | px exact | lossless frames |
|---|---|---|---|---|
| `hybrid` / `pal-lz` | **389** | **0.00** | **100%** | **100%** |
| `cc2-42` | 1281 | 0.84 | 57.6% | 0% |
| `blk42` | 1281 | 1.37 | 57.7% | 0% |
| `cc4` | 1281 | 2.08 | 57.6% | 0% |
| `bc1-dual` | 1297 | 10.69 | 90.7% | 0% |
| `bc1` | 1025 | 11.88 | 53.4% | 0% |
| `ycocg-420` | 1409 | 42.60 | 54.9% | 0% |

`cc2-42` gets to dE 0.84 and *still* only reproduces 58% of pixels exactly,
because its RGB565 colours cannot represent the UI's amber and cyan. For
"12:34" on a clock face, 58% is not a pass.

**`darkfade`** (near-black, a few bright stars) — this is where endpoint
precision decides everything, and where the temporal-dither column bites:

| codec | dE(6-bit) | dE(6-bit +tdith) | px exact |
|---|---|---|---|
| `hybrid` | **0.98** | **1.23** | 73.2% |
| `bc1-dual` | 1.25 | 1.81 | 67.1% |
| `bc1-e888` | 1.26 | 1.81 | 67.1% |
| `pal5-adapt` | 1.99 | 1.83 | 59.0% |
| `bc1` (RGB565 ends) | 3.68 | **13.88** | 0.5% |
| `cc2-42` (RGB565 ends) | 6.02 | **17.68** | 0.2% |
| `cc4` (RGB444 ends) | 10.59 | **35.02** | 0.2% |
| `blk42` (RGB444 ends) | 10.64 | **34.83** | 0.2% |

`bc1` and `bc1-e888` differ *only* in endpoint format — identical block size,
identical index count — and 888 endpoints are **2.9x better** here. Look at
`docs/research/img/sheet-darkfade.png`: the RGB444 modes paint magenta and
purple blotches across what should be near-black sky, because 4-bit sRGB steps
cannot land near the origin. It is the most visible artefact in the whole study
and the point metric agrees with the eye exactly.

### Pictures

* `img/sheet-textui.png` — all 22 codecs on a UI frame. The block codecs wash
  out the progress bar and bleed colour around the glyphs; the palette codecs
  are exact.
* `img/sheet-photo.png` — the stress case. `pal4-adapt` (undithered) has
  visible contour banding in the sky while `pal4-adapt-ord` does not, even
  though the dithered version scores *worse* on plain dE (32.01 vs 28.51) and
  better on `dE blurred` (17.68 vs 18.45). This is the one place the point
  metric and the eye disagree, and it is why `dE blurred` exists.
* `img/sheet-darkfade.png` — the endpoint-precision story above.
* `img/zoom-plasma.png` — the five best on smooth saturated motion, at 9x.
* `img/temporal-dither.png` — two consecutive temporally-dithered frames and
  their 4-frame average against the source average.

---

## What was rejected, and why

**YCoCg with chroma subsampling** — comprehensively the worst family tested
(dE 36-41, an order of magnitude behind the winner) despite spending the most
bytes (1409-1441 B). Two independent failures: at 3 bits the chroma step is 64
out of ±256, which visibly desaturates and adds chroma noise everywhere; at 4x4
subsampling the chroma blocks are plainly visible in the sky of `photo`. At
64x32 there simply is not enough spatial redundancy in chroma to pay for the
luma precision you give up — a frame is 2048 pixels, not two million. Rejected
even though the transform itself is beautifully cheap (adds and shifts only).

**Fixed palettes** — dE 64-81, 6-12x worse than the adaptive equivalent at
identical cost, and the worst flicker in the study (`pal5-fixed`, +4.26): as
content hues drift, pixels flip between fixed palette entries in spatially
correlated clumps. A per-frame palette costs 48-96 bytes. Never worth skipping.

**RGB444 and RGB565 block endpoints** — see `darkfade` above. They are not
merely worse, they are *deceptively* worse: they score well against the panel
we have and badly against the panel we want. Excluded on that basis.

**Floyd-Steinberg error diffusion** — worse than ordered dither on dE (12.44 vs
10.77), much worse on flicker (+3.24 vs +2.41), and it destroys LZ ratio. The
error pattern is content-dependent, so it crawls when content moves. Ordered
dither is temporally stable by construction. On a 30 fps display, stability wins.

**Dithering at all, mostly.** At 5 bpp dither is a wash or a loss on every
metric (overall `pal5-adapt` 9.52 against `pal5-adapt-ord` 10.77; blurred 5.14
against 5.50). At 4 bpp on gradient-heavy content it earns its keep, but only
once you low-pass: on `photo`, `pal4-adapt` scores 28.51 and `pal4-adapt-ord`
32.01 on plain dE, and **17.68 against 18.45 blurred** — the ordering flips.
Look at `sheet-photo.png`: the undithered version has obvious contour bands in
the sky and the dithered one does not. Since we are not shipping a 4 bpp mode,
the recommendation is **do not dither on the sender**, except in the PAL5
floor where the palette is genuinely tight.

**Sender-side temporal dithering** — `pal5-adapt-tdith` rotates the Bayer phase
per frame. It does what it claims: `dE t-avg4` improves (6.51 vs 7.10) and
`img/temporal-dither.png` shows the 4-frame average is visibly smoother than
the static-dither average. But `flicker` is worse (+2.68 vs +2.41), and at 30 fps
a full-amplitude per-pixel pattern change is *visible shimmer*, not integration.
The right place for temporal dithering is the panel driver at 154 Hz, not the
sender at 30 Hz. Card 030.

**heatshrink** — considered and not used. Its LZSS is bit-packed, which needs a
stateful bit reader, and it needs a `2^window` ring buffer of its own. The
byte-aligned LZ here decompresses straight into the destination frame buffer
using the output as its own window: **zero scratch RAM**, and decode is a
compare, a shift and a byte copy per item. heatshrink is ISC-licensed and fine;
it just costs RAM we do not need to spend.

**`imagequant` / `color_quant` / `exoquant`** for palette design — `imagequant`
is GPL-3.0-or-later (dual-licensed commercially), `exoquant` has not been
released since 2016, `color_quant` is NeuQuant from 1994. If you would rather
not carry the hand-rolled median-cut-plus-Lloyd quantiser in `lab/src/enc/quant.rs`
into the sender, **`quantette`** (MIT/Apache, Wu + k-means, already works in
Oklab) is the one to use.

**Larger blocks** (`blk84-i3`, 8x4 with 8 levels, same 1025 B as `bc1`) — dE
16.73 vs 14.09. At 64x32, block size dominates level count. Do not go above 4x4.

---

## ESP32 decode cost

Measured, not estimated. `lab/xtensa-bench` compiles the decoders for
`xtensa-esp32-none-elf` and runs them under `qemu-system-xtensa` with one
instruction per translation block, counting executed instructions. Each mode is
built twice — decoding its payload once and eleven times — and the difference
divided by ten gives one decode with startup, payload load and exit cancelled.

Caveat: qemu's `sim` machine models a dc232b, not the ESP32's LX6. That is fine
for *counting instructions* (this is all base-ISA integer work — no ESP32-only
opcodes are generated) but it says nothing about cycles. The times below bracket
CPI between 1.0 and 1.5; internal-SRAM code and data on an LX6 is close to the
former, code fetched through the flash cache closer to the latter. Card 013
should confirm against `CCOUNT` on real hardware.

Shipping modes in **bold**.

| mode | payload B | instructions/frame | us @240 MHz, CPI 1.0 | us @240 MHz, CPI 1.5 | % of a 33.3 ms frame (CPI 1.5) |
|---|---|---|---|---|---|
| **`pal4-lz`** | 392 | **71 600** | 298.3 | 447.5 | 1.34% |
| **`pal8-lz`** | 1031 | **88 875** | 370.3 | 555.5 | 1.67% |
| **`pal5`** | 1377 | **68 010** | 283.4 | 425.1 | 1.28% |
| **`bc1-dual`** | 1297 | **131 293** | 547.1 | 820.6 | 2.46% |
| `pal4` | 1073 | 45 273 | 188.6 | 283.0 | 0.85% |
| `cc2-42` | 1281 | 43 635 | 181.8 | 272.7 | 0.82% |
| `blk42` | 1281 | 54 641 | 227.7 | 341.5 | 1.02% |
| `cc2-44` | 1025 | 61 763 | 257.3 | 386.0 | 1.16% |
| `bc1-e888` | 1281 | 66 253 | 276.1 | 414.1 | 1.24% |
| `cc4` | 1281 | 67 389 | 280.8 | 421.2 | 1.26% |
| `bc1` | 1025 | 70 249 | 292.7 | 439.1 | 1.32% |
| `blk84-i3` | 1025 | 105 562 | 439.8 | 659.8 | 1.98% |
| `bc1-i3` | 1281 | 127 871 | 532.8 | 799.2 | 2.40% |
| `ycocg-420` | 1409 | 188 498 | 785.4 | 1178.1 | 3.53% |
| `ycocg-410` | 1441 | 290 461 | 1210.3 | 1815.4 | 5.45% |

Four things worth noting:

* The worst shipping mode is **2.46% of a 33.3 ms frame**. Decode cost is not a
  constraint at 30 fps and would not be one at 120 fps. The card's "well under
  5 ms" bar is cleared by about 6x even at the pessimistic CPI.
* **3-bit indices cost roughly twice what 2-bit indices cost** — `bc1-i3`
  128 k against `bc1` 70 k, identical block size and endpoint work — because
  pulling one bit out of each of three bitplanes is three loads, three shifts
  and three masks per pixel. `BC1_DUAL` inherits that on the blocks that choose
  8 levels, which is why it is the most expensive mode we ship. If decode ever
  does get tight, packing 3-bit indices as a byte-aligned bitstream instead of
  bitplanes is the obvious lever; there is no reason to spend that complexity
  now.
* **Decompressing is nearly free.** `pal8-lz` costs 89 k against `pal4`'s 45 k
  and `pal5`'s 68 k, and it is doing an LZ inflate on top of the palette
  expansion. A match is a short byte copy and most frames are dominated by
  matches. Variable-rate coding is not buying quality at the device's expense.
* `ycocg-410` is simultaneously the **most expensive** mode (290 k, 5.45%) and
  among the worst-looking. Five bitplanes per chroma sample plus five for luma
  means ten separate bit extractions per pixel. It loses on every axis.

The counts include the register-window spill/fill traffic that the Xtensa
windowed ABI generates, which the firmware will also pay.

The decoders are proved firmware-ready mechanically: `lab/nostd-check` compiles
`lab/src/dec/` as `#![no_std]` with no allocator declared. Lifting them into
`crates/proto` should be a file move.

---

## What surprised us

1. **The panel's depth, not the byte budget, is the binding constraint for the
   block codecs.** `bc1-dual` scores 11.01 against today's 6-bit panel and 10.61
   against the same panel with temporal dithering — it gets *better* when the
   panel improves, because it is currently throwing precision away. `blk42` goes
   11.30 → 16.47 and `cc4` 12.82 → 18.01 — they get *worse*, because the panel
   was hiding them. Ranking codecs against the display you have rather than the
   display you are building is a trap, and we nearly walked into it.

2. **Six bitplanes plus five-subframe temporal dithering beats eight bitplanes
   flat** (195 vs 183 distinct levels). The cheapest large quality win available
   to this project is in the panel driver, not the codec.

3. **Palette-plus-LZ beats every block codec on smooth saturated motion.** We
   expected BC1-style blocks to own `plasma` and `mandel`; instead PAL8_LZ was
   picked for 98% and 100% of their frames. At 2048 pixels a frame usually has
   under ~1000 distinct colours, so a 256-entry palette is nearly lossless and
   the index plane compresses well. The tiny frame size changes the answer
   relative to the texture-compression literature, where BC1 is the obvious pick.

4. **Text costs 389 bytes.** A whole class of content fits in a quarter of the
   budget, bit-exact. Any design that spends a fixed 1297 B on a clock face is
   wasting three quarters of its packet.

5. **Ordered dither loses on every number and wins to the eye** in exactly one
   place — 4 bpp smooth gradients. The `dE blurred` metric was added *after*
   looking at `sheet-photo.png` and disagreeing with the table; it flips the
   ordering there and essentially nowhere else, which is a good sign for the
   metric and a reminder to look at the pictures.

6. **Floyd-Steinberg is a temporal liability**, and no per-frame metric shows it.
   Only the flicker column does.

---

## Open questions

* **Does the hybrid's encode cost fit 30 fps on the sender?** It runs three
  encodes plus three decodes plus three scorings per frame. In the lab that is
  roughly 8 ms per frame single-threaded on an M-series laptop — inside budget,
  but the lab encoder is not optimised and a Raspberry Pi sender would be much
  tighter. Card 031.
* **Does the ranking hold on real photographs and video?** All content here is
  synthetic. The `photo` generator is broadband and detailed but it is not a
  camera. Card 032.
* **Should the palette ladder consider a dithered rung?** It currently drops
  dithering as soon as it starts squeezing, because dither costs LZ ratio. On a
  frame that just misses the budget, a dithered 32-colour palette might beat an
  undithered one. Not measured.
* **Is `MARGIN = 0.92` right?** The hysteresis threshold was set by judgement,
  not measurement. Measuring mode-switch visibility needs the camera harness
  (card 012).
* **Per-block mode switching in the palette modes?** `BC1_DUAL` showed that one
  flag bit per block is cheap and effective. Nothing analogous was tried for the
  palette family.
* **What does a real packet-loss trace look like?** Every mode here is
  stateless, so loss costs exactly one frame by construction — but we never
  measured what a 1-5% loss rate looks like to the eye at 30 fps. Card 013.
