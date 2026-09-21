# Brief: making generative art for the screeny panel

You are building a generative art system whose final output is a 64x32 RGB LED
matrix (a Tidbyt Gen 1 running our custom "screeny" firmware), fed over WiFi by a
sender program at up to 30 frames per second. This brief tells you what that target
can and cannot show, how frames get there, and how to design for it. It was written
by the Claude instance that is building the firmware and sender.

Every statement carries a status:

- **[measured]** observed on the real device.
- **[decided]** a design decision that will not change without notice.
- **[provisional]** our current best understanding; expect revision. Design so that
  a change here does not break you.

Source of truth as the project moves: `docs/design/protocol-v1.md` (wire
protocol), `docs/research/001-firmware-stack.md` (hardware), `docs/research/002-*`
(frame encoding lab and codec measurements), `docs/research/004-first-bringup.md`.
If this brief disagrees with those, they win; tell your user so this file gets fixed.

---

## 1. The target at a glance

| | | |
|---|---|---|
| Resolution | 64 x 32, 2048 pixels, 2:1 | measured |
| Pixel | discrete round RGB LEDs on a black mask, roughly 3 mm pitch, dark gaps between them, no diffuser | measured |
| Black | LED off. True black, effectively infinite contrast | measured |
| Colour depth | **6 bit planes per channel, linear light, plus device-side temporal dithering** across the 154 Hz refresh: darkest visible level is about sRGB 6 (it was 34 without dithering) | measured |
| Brightness | runtime 0-255 via LED on-time, **does not cost colour depth**; 25 real steps; firmware cap 160, default 96 | measured |
| Frame rate | **30 fps, and nothing else** (card 161): every codec ran 60 s at 30 with zero decode drops and 0.2-0.9% network loss. The firmware stayed clean up to 120 fps on the bench, so faster is *possible* - but WiFi loss and jitter set the ceiling, the picture gains nothing, and the owner asked for one rate with no variability. `screeny_art::FPS` is it | measured, then decided |
| Transport | one frame = one UDP datagram, **1464 bytes** for all 2048 pixels (~5.7 bits/pixel) | decided |
| Delivery | unreliable, newest frame wins, a lost frame is simply skipped; the panel holds the last frame | decided |
| Latency | ping RTT median 5 ms, p99 21 ms, max 37 ms; decode 0.2-0.8 ms; inter-arrival jitter 2-5 ms | measured |
| Device state | none. Every frame is a complete picture. No blending, trails or accumulation on the device | decided |

The three facts that should shape everything you do: **it is tiny, its dark end is
coarse, and a frame has to fit in 1464 bytes.**

---

## 2. Colour: what the panel can actually show

### 2.1 Linear light, 64 hardware levels, dithered in time

The panel modulates each LED in linear light with 64 hardware levels per channel.
You author in sRGB like everyone else; the firmware converts with a gamma table.
The levels are very unevenly spaced in sRGB terms. The sRGB value (0-255) of each
hardware level, dark to bright:

```
0 34 50 62 71 80 87 94 100 106 111 116 121 125 130 134 138 142 146 149 153 156
160 163 166 169 172 175 178 181 183 186 189 191 194 197 199 201 204 206 209 211
213 215 218 220 222 224 226 228 230 232 234 236 238 240 242 244 246 248 250 251
253 255
```

On its own that would mean anything under sRGB ~22 is off and the darker half of
sRGB is only 14 levels. **The firmware now fills those gaps by temporal dithering**:
it carries the sub-level remainder across panel refreshes (154 Hz, about 5 per 30 fps
frame), so in-between values are shown as a time average. **[measured, card 007]**:
the darkest visible value moved from sRGB 34 to about 6, mean luminance wobble is
1.7% with no periodic structure, and nothing is visible as flicker to the eye **from
across a room**. **[owner, by eye, 2026-09-21]**: within a few feet, held colours close
to black visibly blink at several hertz - that 1.7% is an average over the panel, and
up close the eye resolves single pixels. Section 2.1.1 is what to do about it from the
sender's side; card 248 is the firmware's side.

**How much resolution that is, exactly [measured, card 102]**: the firmware keeps four
fractional bits below a duty level and spends the remainder over a **16-phase** cycle,
one phase per refresh, with a Bayer 4x4 offset per pixel so the panel does not beat in
unison. So a colour that is *held* - from about 104 ms, three 30 fps frames - averages
1008 duty steps per channel, of which the 256 sRGB codes reach 237, and only sRGB 0 and
1 emit nothing at all. A colour on screen for a single frame gets about five of those
sixteen phases and resolves ~195 levels, which is the number card 002 measured and what
the codec chooser scores against. The model is `screeny_panel::DEVICE`, checked entry by
entry against the firmware's own gamma table.

What that changes for you, and what it does not:

- Dark gradients are **much better than the table suggests, but still the panel's
  weakest range.** The time average is built from few refreshes, so very dark values
  (under sRGB ~30) are a sparse sparkle of single-level blinks rather than a steady
  dim glow; large dark areas can look faintly alive. Slow fades to black are now
  usable; long-held near-black detail is still not where to put your subtlety.
- Put tonal detail in the middle and upper range, use true black as a shape, and
  prefer fades that are either reasonably quick or that also shrink/erode the shape.
- **You do not need to dither in time yourself** to gain levels, and should not: the
  device does it at 154 Hz, which is invisible; anything you do at 30 fps is 15 Hz
  and visible. Spatial (ordered) dither is still yours to use as texture (2.4).
- **Brightness no longer costs depth.** It is set by LED on-time per scan line, not
  by scaling values, so the full level structure survives at any brightness. It is a
  runtime control (`screeny brightness N`), 25 real steps, capped in firmware.
- A camera cannot photograph the dithered panel honestly (a 1/30 s exposure against
  a ~100 ms dither cycle), and the bench camera's colour response is unknown. Judge
  by eye on the device; do not tune art to camera captures.

### 2.1.1 Held dark shades: put them on a panel level

A colour that sits exactly on one of the 64 hardware levels is lit the same way on every
refresh. It cannot blink, with the device's dither on or off. A colour between two levels
is made by alternating them over a 16-refresh cycle, 9.6 Hz, and near black the two
levels are "off" and "on": that is the blinking. So the rule is about **where a shade
sits**, not about how many shades you use:

- **Align what is held and dark.** Unlit clock segments, ghost dots, dim backgrounds,
  rules, the bottom entries of a palette: anything that stays on screen for more than a
  few frames with a channel under about sRGB 140. Leave everything else to the dither -
  moving content, quick fades, and anything brighter, where a level is at most four
  codes wide and the alternation is a few percent of the light.
- **The aligned codes, dark end** (the lowest sRGB code that lands on each level, from
  the firmware's own gamma table; every one is within 2/16 of a level, and correct
  whether the device dither is on or off):

  ```
  level   0   1   2   3   4   5   6   7    8    9   10   11   12   13   14   15   16
  sRGB    0  34  50  62  71  80  87  94  100  106  111  116  121  126  130  134  138
  ```

  That is the whole budget: **nine steady values per channel at or under sRGB 100.**
  (The table in 2.1 is the *nearest* code to each level's light, which is the right
  question for a preview; twenty-two of its entries sit a hair under their level - 125,
  149, 156 and others - and fall to the level below if the device is not dithering. For
  choosing a code to send, use this rule instead: the lowest code that reaches the level.)
- **Alignment is per channel, so the darkest shades lose their hue.** A warm grey is
  three channels on three different levels, and at the bottom the only choices are 0 and
  1: the darkest steady colours are the seven combinations of level-1 primaries, then
  things like (2,1,1) and (2,2,1). Design a dark ramp as a short list of **level
  triples**, look at each one on the panel, and keep the ones that read as the same
  material. Four deliberate dark shades beat twelve computed ones.
- **Snap after you blend, not before.** Anti-aliasing, glow falloff, cross-fades and
  linear-light blending all manufacture in-between values. Bright edges are fine. A
  *held* dim edge or halo is exactly the pixel that blinks: for any channel that ends up
  below level ~16, move it to the nearest level as the last step.
- **Need a shade between two dark levels? Mix them in space, not in time.** A fixed
  ordered pattern of the two adjacent aligned levels (2.4) is perfectly steady. It reads
  as texture up close, which suits a dot-matrix picture better than a blink does.
- **Buy levels with brightness.** Light is level x brightness, and brightness costs no
  depth (above). If a patch's brightest value is well under full - a clock whose lit dots
  are sRGB ~175 is only using 27 of the 63 levels - scale the patch up in linear light
  and turn the panel's brightness down by the same factor. The picture is as bright as
  before and everything dark has moved up onto about twice as many levels. Brightness
  has 25 real steps and a floor, so this is a coarse trade, made once per patch.
- **How, in this repo (card 188).** The table above is
  `screeny_panel::aligned_levels(max_level)` - derived from `screeny_panel::DEVICE`
  itself, not typed in, so a firmware change to the gamma table only has to change that
  one model. `screeny_panel::duty_16ths(code)` and `screeny_panel::nearest_level(duty)`
  are the two pieces it is built from: a code's duty in sixteenths of a level, and any
  duty's nearest level with a signed offset.

  `output.panel: bit_planes` (`screeny_art::panel::Panel::BitPlanes`) is still the
  blunt tool - steady everywhere, but it also flattens the bright gradients the dither
  handles well. The third choice is `output.panel: aligned_dark`
  (`screeny_art::panel::Panel::AlignedDark`): dithered above
  `screeny_art::panel::DARK_ALIGN_LEVEL` (16, a named constant - see below), forced onto
  the nearest aligned code below it. It is the backstop: whatever a patch has not aligned
  itself - a stray anti-aliased edge, a treatment left deliberately undimmed - still
  cannot blink once this is the panel a frame is quantised through.

  A patch that wants its *own* dark ramp - the better-looking fix, chosen by eye rather
  than left to per-channel rounding - builds level triples with
  `screeny_art::panel::level_triple([r_level, g_level, b_level]) -> Rgb` (each channel is
  `screeny_art::panel::level_code(level)`, decoded back to linear light) and applies them
  as the *last* step, after blending, exactly where "snap after you blend" above says to.
  `crates/art/src/patches/clocks` is the worked example: `DARK_RAMPS`, two short,
  hand-picked lists of level triples for the resting dials' held ink, selectable in the
  studio via a `choice("dark", ...)` parameter the same way any other named choice is
  (card 163) - applied only once a dial has actually landed, never mid-fade, so a fade
  stays the cheap continuous ramp the "leave everything else to the dither" bullet asks
  for and only the truly held colour is aligned.
- **This table will change.** Card 248 gives the firmware steadier sub-levels below
  level 1 and a dither that does not run at 10 Hz. When it lands there are more steady
  dark values and the penalty for missing one is smaller. Read the levels from
  `screeny_panel`, never from a constant of your own - `duty_16ths`, `nearest_level` and
  `aligned_levels` are built from `screeny_panel::DITHER_PHASES`, not a literal `16`, so
  248 has exactly one place to change them. `DARK_ALIGN_LEVEL` in `crates/art` may come
  down with the shorter cycle; it is a named constant for the same reason.

### 2.2 The primaries are not sRGB

LED primaries are narrow-band and more saturated than sRGB's. **[measured by eye
through a camera, not with a colorimeter]**:

- Saturated colour is where this display is spectacular. Pure hues on black glow.
- "White" is three LEDs and reads cool/bluish. Greys are not reliably neutral, and
  low greys are the worst case for the level problem above (three channels each
  stepping at different sRGB values produce colour casts). Avoid large areas of
  subtle grey or pastel; they will look dirty and banded.
- Perceived brightness differs a lot by hue: green is brightest, red moderate, blue
  dim and slightly violet. Secondaries (yellow, cyan, magenta) light two LEDs and
  are brighter than primaries; white lights three. A hue rotation at constant RGB
  "value" pulses visibly in brightness. If you want even brightness across hues,
  design in a perceptual space (OKLCH) and convert, then check on the device model.
- Do all blending, anti-aliasing, blurring and downsampling **in linear light**
  (decode sRGB, do the maths, re-encode). On this display the error from blending in
  gamma space is large and shows up as dark fringes on moving edges.

### 2.3 The byte budget is really a palette budget

2048 pixels in 1464 bytes. Raw RGB888 would be 6144 bytes, so every frame is
compressed. The sender picks a codec per frame (spec: `docs/design/protocol-v1.md`
section 4; measurements: `docs/research/002-frame-encoding.md`). **[decided]** What
that means for you:

- **A frame with at most 16 distinct colours is always exact.** Text/UI-like frames
  land around 400 bytes.
- **A frame with at most 32 distinct colours is always exact** (the fixed-size
  `PAL5` floor, 1376 bytes).
- **A frame with up to 256 distinct colours is exact *if its index image compresses*
  into the budget** (`PAL8_LZ`: palette + LZ-compressed indices). Smooth, coherent
  images compress well: in the lab a plasma and a Mandelbrot zoom went out this way
  on 98-100% of frames. Noise, fine dither and high-frequency texture do not
  compress, and the sender then reduces the palette (256 -> 128 -> 64 -> 32) until
  it fits.
- Anything else is sent lossy, either as a reduced adaptive palette or as a 4x4
  block codec (`BC1_DUAL`) which suits photographic content. The sender scores the
  candidates perceptually and picks the best; you do not choose.
- The palette is per frame. It can be any colours at all, and it can change
  completely on every frame at negligible cost.

Two consequences worth internalising:

1. **Spatial coherence is your compression.** Large flat or smoothly varying regions
   are nearly free; per-pixel noise is the most expensive thing you can draw. This is
   the opposite of the intuition from the colour-count rules alone: a 200-colour
   smooth gradient fits exactly, while a 40-colour field of random confetti may not.
   Ordered dither (section 2.4) is a regular pattern and compresses far better than
   random or error-diffusion dither.
2. **Think like an indexed-colour artist.** This target rewards the whole family of
   palette techniques:
   - *Palette animation*: keep the index image fixed or slow-moving and animate the
     palette (colour cycling, palette rotation through a fractal, day/night shifts).
     Perfectly lossless and nearly free.
   - *Deliberate palettes*: choose 8-32 colours per patch or per scene, as a design
     decision, in a perceptual space, snapped to panel levels (section 2.1). Render
     directly into indices. You control quantisation instead of a generic quantiser
     guessing, and the result is exact on the wire.
   - *Ramps*: a 32-entry palette is one 32-step ramp, or four 8-step ramps in
     different hues, or 8 hues x 4 brightnesses. Pick the structure the patch needs.

If a patch genuinely needs continuous colour (a photographic source, a many-hue
field), it will still work, lossy, at a quality the lab measured as good (SSIM
~0.98). But prefer designing within a palette: it will look better *and* be exact.

**Palette stability** **[measured in the lab]**: when the sender has to build a
reduced palette, it seeds it from the previous frame's and applies hysteresis to
codec switching, because both palette drift and a change in the *character* of the
error (block edges vs dither noise) are visible as shimmer. If you own the palette,
neither can happen. If you hand over full-colour frames, keep slow gradients moving
or textured rather than nearly static.

### 2.4 Dithering

You have few levels and few colours, so you will want dither. At this size it is
very visible; treat it as a texture you are choosing, not a hidden trick.

- Use **ordered dithering with a fixed pattern** (Bayer 4x4 / 8x8, or a 64x32
  blue-noise mask). The pattern stays put from frame to frame, so it reads as a
  stable screen-door texture.
- Avoid error diffusion (Floyd-Steinberg etc.) for animation. The pattern depends on
  the whole image, so it crawls and boils when anything moves.
- Temporal dithering (alternating two adjacent levels on successive frames) works,
  but at 30 fps the alternation is 15 Hz, which is visible as flicker on bright
  LEDs, especially in peripheral vision. Keep it to one quantisation step of
  amplitude, and vary the phase per pixel with a blue-noise mask so the whole field
  never flickers in unison. **You should rarely need it: the firmware already
  dithers in time at the 154 Hz refresh rate, invisibly (section 2.1).**
- Dither in linear light, against the real panel levels, not against 8-bit sRGB.

---

## 3. Space: designing for 2048 pixels

- Every pixel is individually visible. At a normal viewing distance (1-3 m) the eye
  merges the LEDs into an image, but there is no such thing as fine detail. Features
  smaller than 2 px are noise. Favour big shapes, bold contrast, strong silhouettes.
- **Supersample everything.** The frame is so small that you can render at 8x
  (512x256) or 16x and box- or Gaussian-filter down in linear light, for free.
  This is the difference between a fractal zoom that shimmers and one that flows.
- **Sub-pixel motion is essential.** One pixel is about 1.5% of the width. Anything
  moving slower than ~1 px/frame will visibly step unless you render it with
  anti-aliased sub-pixel positions (which supersampling gives you automatically).
  Anti-aliasing works well here as long as the edge is bright-on-dark, because the
  intermediate values land in the well-populated upper range of section 2.1.
- Thin lines: a 1 px line at an angle is a staircase of separate dots. Use >= 1.5 px
  anti-aliased strokes, or embrace the pixel grid and keep lines axis-aligned or 45 degrees.
- Circles below ~4 px radius are octagons. Small particles are better as soft
  anti-aliased blobs (a 3x3 Gaussian footprint) than as single pixels, and single
  pixels twinkle as they cross pixel boundaries unless sub-pixel rendered.
- Negative space is free and beautiful: off LEDs are truly black, cost nothing in
  the byte budget (long runs of one index compress to almost nothing in the lossless
  modes), and keep average brightness down. Dark backgrounds are the house style.
- Text, if you use it: a 5x7 font gives about 10 characters x 4 lines; 3x5 is the
  legibility floor and only for upper case and digits. Hand-made bitmap fonts only;
  never rasterise an outline font at this size.
- The aspect ratio is 2:1. Radially symmetric patches waste a third of the panel
  unless you let them overflow. Compose for wide.

---

## 4. Time

- **30 fps is the ceiling, not a guarantee.** Frames can be dropped (a few percent on
  a bad WiFi day) and arrive with 10-25 ms of jitter. So:
  - Drive all animation from **elapsed wall-clock time `t`**, never from a frame
    counter. A dropped frame must cause a skip, not a slowdown.
  - Every frame must be a **complete, self-contained image**. If the patch uses
    feedback (trails, reaction-diffusion, cellular automata, accumulation buffers),
    keep that state in *your* process and send the rendered result. The device never
    sees deltas.
  - Do not rely on frame-exact effects (single-frame flashes, 2-frame strobes,
    anything that must not be missed). A one-frame event should last at least 3
    frames if it matters.
- Speed feel: 1 px/frame is the full width in ~2 s, which is brisk on this display.
  Ambient patches want much slower motion, which is why sub-pixel rendering matters.
- The panel itself refreshes at 154 Hz with no tearing at frame swaps **[measured]**,
  so what you send is what is shown; there is no vsync for you to chase.
- **Photosensitivity: no full-field flashing above 3 Hz**, and avoid large-area
  high-contrast red flashes entirely. The panel is very bright and saturated. Build
  this in as a limiter on global luminance change per frame, not as a guideline to
  remember.
- Average brightness matters. The panel is USB-powered with a firmware brightness
  cap **[decided]**. A mostly-white frame is harsh to look at and is where any power
  limiting would bite first. Aim for an average picture level under ~40% and let
  highlights be small.
- Audio-reactive work is in scope (the owner wants to use this as a visualizer). The
  display path adds roughly 50 ms worst case (up to 33 ms frame wait plus network),
  so keep capture and analysis under ~20 ms (512-1024 sample windows at 48 kHz) and
  the result will feel locked to the music.

---

## 5. How your system should hand over frames

The sender exists. **[decided]** It is `crates/screeny`, it is a library before it
is a CLI, and you link it: `Output` becomes fifteen lines and there is no pipe,
no subprocess and no framing to agree on. `crates/screeny/README.md`'s
**Embedding** section is the reference; `crates/screeny/examples/art_output.rs`
is a working sketch of the impl written against your `Output` and `WireFrame` as
they stand. **[built, card 101]** It is now `crates/art/src/output/sender.rs`
(`SenderOutput`, behind the `sender` feature) and `screeny-art play <patch> --to
NAME|ADDR`.

```rust
use screeny::{Link, LinkConfig, Pixels, Target};

let mut link = Link::open(Target::default(), LinkConfig::default())?;
loop {
    let wire = pipeline.process(patch.frame(ctx), dt);     // your loop, your clock
    link.send(match &wire.indexed {
        Some((palette, indices)) => Pixels::indexed(palette, indices),
        None => Pixels::rgb(&wire.rgb),
    })?;
}
```

Seven things about it that should change how you build the output stage.

1. **Indexed frames really do go on the wire exactly.** A palette of 32 colours or
   fewer plus 2048 indices reaches the panel as `palette[index]`, every pixel, no
   requantisation and no dither of ours on top of yours. **[measured]** end to end
   against the reference receiver for palettes of 2, 16, 17 and 32 colours, with
   structured indices *and* with pure index noise. The promise holds for any index
   plane because the floor of the exact path is a fixed-rate 1376-byte codec that
   cannot overflow. This is `overland`'s whole premise and it is safe to build on.
2. **Up to 256 colours are exact when they compress.** The 32 in section 2.3 is the
   size that is *guaranteed*; a larger palette still goes out losslessly whenever
   the LZ coder can fit it, which for flat-shaded and terraced work it usually can.
   Ask `Link::limits().exact_palette` for the guaranteed size against the device you
   are actually connected to.
3. **When nothing exact fits you are told, not fooled.** Too many colours and too
   little structure means the frame is expanded and run through the lossy chooser -
   a requantised frame beats a dropped one - and `Sent::exact()` comes back false
   with `LinkStats::indexed_fallback` rising. It is on the studio's stats strip next
   to the four numbers, which is where the stand-in `budget.rs::simulate_lossy`
   used to be. **[done, card 101]**
4. **Estimates are gone; the numbers are measured.** `Sent::bytes()` is the actual
   payload size and `Sent::codec()` the codec that carried it. Nobody models our
   encoder any more; they ask it. `crates/art/src/meter.rs` does exactly this -
   `screeny::encode::Encoder` with no network anywhere, then `screeny_proto`'s
   decoder - so the studio's frame size, codec and **preview picture** are what the
   panel will really do, with or without a panel present. `budget.rs` is deleted.
   **[done, card 101]**
5. **Render at `screeny_art::FPS`, which is 30.** Pacing is still yours - the link
   never sleeps - and by default it drops frames that arrive before the panel's
   next slot rather than sending them, on an absolute schedule. At the panel's own
   rate there is nothing to drop: the device's superseded counter stays at zero and
   so does `frames_coalesced`. **[measured]**

   This paragraph used to say the opposite - "keep your 60 fps loop, do *not* solve
   this by rendering at 30, let the link decimate" - and card 161 reversed it on
   the owner's instruction: the display can do nothing with the extra frames, so
   half the rendering was being thrown away and a patch's motion was sampled at 60
   and shown at 30. **Step your patch by `ctx.dt`, not by a frame count**, and the
   rate is then something the system can change without changing your patch.
   `Link::fps()` is still the rate the panel is actually keeping up with, which
   moves: sustained packet loss steps it down and back up by itself (spec 6.9).
6. **The panel going away is not your problem.** `Link::send` cannot fail because of
   the network. A device that reboots, drops off WiFi or changes address is a run of
   `Sent::Dropped` and a counter; the link re-resolves (by mDNS name, so it follows a
   DHCP lease), reopens a socket and resumes, on a background thread so your loop
   never stalls. `Link::open_deferred` starts with no panel at all, which is what a
   server process wants. The only errors `send` returns are yours: a frame of the
   wrong size or an index outside its palette.
7. **Let the link drop out of scope when you are done.** That sends `FINAL` and the
   panel releases the source lock at once instead of holding your last frame for ten
   seconds. It does *not* happen on `SIGTERM` or ctrl-c, which skip destructors, so
   install a handler if you care about that second.

The raw-RGB pipe still exists (`screeny pipe`), unchanged, and is still the right
answer for anything not written in Rust. It is lossy above 32 colours by
construction, so it is the wrong answer for you.

**The hardware still has a single owner.** Linking the sender does not change that:
do not point a stream at the bench device, and never open the serial port or the
camera (see `CLAUDE.md`). Develop against `crates/sim` - `cargo run -p screeny-sim`
is a window that shows what the panel would show, and `SimDevice::start(Config::
for_test())` is a real receiver on loopback for your own tests - and against your
own preview:

### Build a faithful preview first

Your most important tool is a simulator that shows what the panel will show, not
what your framebuffer contains. It should:

1. Take your frame (RGB or indexed).
2. Apply the **panel model**: sRGB -> linear -> quantise each channel -> back to sRGB
   for display. **[built, card 102]** `screeny_panel::DEVICE` is that model and
   `crates/art/src/panel.rs` is the art side's reading of it, so nobody writes a
   second one. A held colour resolves 1008 duty steps (237 of the 256 codes); the
   preview also applies the dark end's *collapse*, where up to four codes below sRGB
   39 share a level. `Panel::BitPlanes` is the same panel without the temporal
   dither - 64 levels, nothing under sRGB 34 - and the studio offers both, because
   the darks behave like the coarser number for anything that does not hold still.
   Optionally apply the lossy path so you see codec damage too; since card 101 that
   is the real encoder and real decoder rather than a model of them.
3. Draw each pixel as a **round dot on black with gaps** (dot diameter ~60-70% of
   pitch), upscaled at least 12x. A plain nearest-neighbour upscale lies to you: it
   makes dither look coarser and thin lines look more solid than the real thing.
   A slight bloom on bright dots gets closer still.
4. Offer a "squint" view: the same thing blurred, to approximate viewing distance.

Judge every patch in that preview, at actual physical size on your screen if you
can (the panel is about 19 x 10 cm), not zoomed to fill a monitor.

A statistics overlay is worth having: distinct colours this frame, estimated encoded
size against 1464 bytes, average picture level, maximum frame-to-frame luminance
change. Those four numbers catch most problems before the hardware does.

---

## 6. What works, what does not

Plays to the panel's strengths:

- Palette-cycled anything: plasmas, fractal zooms with rotating palettes, flow
  fields rendered into index ramps, classic demoscene effects (rotozoomers, tunnels,
  metaballs, fire, copper bars). This display is a demoscene target; lean in.
- High-contrast geometry on black: particles, Lissajous and spirograph curves,
  orbiting bodies, cellular automata, Truchet tiles, L-systems at large scale.
- Slow, supersampled, saturated gradients in the mid-to-bright range, with ordered
  dither as deliberate texture.
- Reaction-diffusion, fluid and wave simulations computed at higher resolution and
  downsampled, mapped through a designed palette.
- Audio spectra and waveforms: bars and rings are naturally low-resolution forms.

Fights the panel:

- Dark, low-contrast, subtle-tonal work; slow fades to black; fog; film grain.
- Photographic or video sources (they work, lossy, and look like what they are: 2048
  pixels of a photo).
- Pastel and near-grey palettes; skin tones.
- Fine linework, small text, 1 px detail, anything relying on more than ~20 visible
  rows of distinct information.
- Full-frame noise or confetti with many hues: the worst case for the byte budget and
  it just reads as static.
- Error-diffusion dither in motion; whole-field temporal dither.

---

## 7. Practical notes

- Compute is not a constraint. 2048 pixels x 30 fps is ~61k pixels/s. You can afford
  64+ samples per pixel, raymarching, per-pixel iterative maths, multi-pass
  simulation. Spend it on supersampling and on rendering in linear light.
- Seed your randomness and log the seed, so a good run can be reproduced and refined.
- Separate *patch* (a pure function of time, seed and parameters -> frame) from
  *output* (preview, pipe, sender). It makes patches testable and lets one process
  render to the preview and the panel at once.
- Build in a global brightness/limiter stage at the end of your pipeline (average
  picture level cap, flash limiter from section 4). Patches should not each have to
  remember the rules.
- When something looks wrong on the device but right in your preview, that is a bug
  in the preview's panel model or in our pipeline. Capture the frame bytes that
  produced it and report it; do not tune the art around it.
