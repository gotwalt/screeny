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

Source of truth as the project moves: `docs/design/protocol-v1-draft.md` (wire
protocol), `docs/research/001-firmware-stack.md` (hardware), `docs/research/002-*`
(frame encoding; not finished at the time of writing), `docs/research/004-first-bringup.md`.
If this brief disagrees with those, they win; tell your user so this file gets fixed.

---

## 1. The target at a glance

| | | |
|---|---|---|
| Resolution | 64 x 32, 2048 pixels, 2:1 | measured |
| Pixel | discrete round RGB LEDs on a black mask, roughly 3 mm pitch, dark gaps between them, no diffuser | measured |
| Black | LED off. True black, effectively infinite contrast | measured |
| Colour depth | **6 bits per channel, linear light** (64 PWM levels), 154 Hz refresh | measured |
| Frame rate | 30 fps ceiling; ~29 fps sustained with zero drops in testing | measured |
| Transport | one frame = one UDP datagram, **1464 bytes** for all 2048 pixels (~5.7 bits/pixel) | decided |
| Delivery | unreliable, newest frame wins, a lost frame is simply skipped; the panel holds the last frame | decided |
| Latency | tens of ms end to end; jitter ~10-25 ms | provisional |
| Device state | none. Every frame is a complete picture. No blending, trails or accumulation on the device | decided |

The three facts that should shape everything you do: **it is tiny, it is 6-bit
linear, and a frame has to fit in 1464 bytes.**

---

## 2. Colour: what the panel can actually show

### 2.1 Sixty-four linear levels per channel

The panel modulates each LED in linear light with 64 levels. You will author in
sRGB like everyone else, and the pipeline converts, but you must know where the
levels fall, because they are very unevenly spaced in sRGB terms. The sRGB value
(0-255) of each linear level, from dark to bright:

```
0 34 50 62 71 80 87 94 100 106 111 116 121 125 130 134 138 142 146 149 153 156
160 163 166 169 172 175 178 181 183 186 189 191 194 197 199 201 204 206 209 211
213 215 218 220 222 224 226 228 230 232 234 236 238 240 242 244 246 248 250 251
253 255
```

Consequences **[measured arithmetic, provisional on the final gamma curve]**:

- Any channel value below sRGB ~22 is **off**. sRGB 0-63 collapses to 4 levels;
  0-127, the entire darker half of what you are used to, is only 14 levels.
- The top end is the opposite: above sRGB ~180 there is a panel level every 2-3 sRGB
  steps. Bright gradients are smooth. Dark gradients band hard.
- So: **do not build pieces that live in the shadows.** A slow fade to black will
  visibly stair-step and then snap off. Moody low-key palettes turn into posterised
  mud. Put your tonal detail in the middle and upper range, and use true black as a
  shape, not as the bottom of a gradient.
- Fades: fade through a dither (section 2.4) or make them fast (under ~300 ms) so the
  steps are not individually seen. Or fade by shrinking/eroding shapes instead of
  dimming them.

Brightness makes this worse **[provisional]**: today the only way the firmware can
dim the panel is to scale values, which throws levels away (50% brightness leaves
~33 levels, 25% leaves ~17). Fixing that is on our board (card 020). Until it is
fixed, assume you may be running with as few as ~32 levels per channel in a dim
room. Art that survives 5 bits per channel is safe.

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
compressed, and the codec is chosen per frame. The exact codec set is being
finalised (card 002) **[provisional]**, but the shape of the constraint will not
change, so design to the shape:

- **A frame with at most 16 distinct colours always fits, exactly**, with room to
  spare (4 bits/pixel = 1024 bytes + a 48-byte palette).
- **A frame with at most 32 distinct colours fits exactly** (5 bits/pixel = 1280 +
  96 bytes).
- A frame with more colours than that is encoded **lossily** (adaptive palette with
  dithering, or a block codec, whichever the sender judges best). How good it looks
  depends on content. Smooth full-frame multi-hue gradients are the hardest case;
  flat regions, limited hue ranges and hard edges are the easiest.
- The palette is per frame. It can be any 16/32 colours out of the panel's
  64x64x64, and it can change completely on every frame at negligible cost.

That last point is the single most useful creative fact here. **Think like an
indexed-colour artist.** This target rewards the whole family of palette techniques:

- *Palette animation*: keep the index image fixed or slow-moving and animate the
  palette (colour cycling, palette rotation through a fractal, day/night shifts).
  Costs 48-96 bytes per frame and is perfectly lossless.
- *Deliberate palettes*: choose 8-32 colours per piece or per scene, as a design
  decision, in a perceptual space, snapped to panel levels (section 2.1). Then
  render directly into indices. You control quantisation instead of a generic
  quantiser guessing, and the result is exact on the wire.
- *Ramps*: a 32-entry palette is, for example, one 32-step ramp, or four 8-step
  ramps in different hues, or a 2D ramp of 8 hues x 4 brightnesses. Pick the
  structure that matches the piece.

If a piece genuinely needs continuous colour (a photographic source, a many-hue
plasma), it will still work, lossy. But prefer designing within a palette: it will
look better *and* be exact.

**Palette stability** **[provisional]**: when a lossy adaptive palette is recomputed
every frame, colours of static regions can shimmer as the palette shifts under them.
If you own the palette, this cannot happen. If you hand over full-colour frames,
expect some shimmer in slow-moving gradients and keep them moving or textured.

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
  amplitude, prefer it in dark/mid tones, and vary the phase per pixel with a
  blue-noise mask so the whole field never flickers in unison. **[provisional: the
  firmware may later do sub-frame temporal dithering itself at the 154 Hz refresh
  rate, which would be invisible. Do not depend on it.]**
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
- The aspect ratio is 2:1. Radially symmetric pieces waste a third of the panel
  unless you let them overflow. Compose for wide.

---

## 4. Time

- **30 fps is the ceiling, not a guarantee.** Frames can be dropped (a few percent on
  a bad WiFi day) and arrive with 10-25 ms of jitter. So:
  - Drive all animation from **elapsed wall-clock time `t`**, never from a frame
    counter. A dropped frame must cause a skip, not a slowdown.
  - Every frame must be a **complete, self-contained image**. If the piece uses
    feedback (trails, reaction-diffusion, cellular automata, accumulation buffers),
    keep that state in *your* process and send the rendered result. The device never
    sees deltas.
  - Do not rely on frame-exact effects (single-frame flashes, 2-frame strobes,
    anything that must not be missed). A one-frame event should last at least 3
    frames if it matters.
- Speed feel: 1 px/frame is the full width in ~2 s, which is brisk on this display.
  Ambient pieces want much slower motion, which is why sub-pixel rendering matters.
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

The sender is being built now; its final interface is **[provisional]**. Two
hand-over formats are planned. Build your output stage behind a small trait so
either can be plugged in.

1. **Raw RGB frames on a pipe** (will certainly exist): 64x32, row-major, top-left
   origin, 3 bytes per pixel sRGB R,G,B = 6144 bytes per frame, written to stdout
   at your own pace up to 30 fps. The sender quantises, picks a codec, paces and
   transmits. Simple, language-agnostic, lossy whenever you exceed 32 colours.
2. **Indexed frames** (planned, preferred for art you control): a palette of up to 32
   sRGB colours plus 2048 indices. This goes on the wire exactly as you made it. If
   your system is written in Rust it will be able to link the sender library
   directly and skip the pipe.

Until the sender exists, do not try to talk to the device, and never open the serial
port or the camera: the hardware has a single owner (see `CLAUDE.md`). Develop
against your own preview:

### Build a faithful preview first

Your most important tool is a simulator that shows what the panel will show, not
what your framebuffer contains. It should:

1. Take your frame (RGB or indexed).
2. Apply the **panel model**: sRGB -> linear -> quantise each channel to 64 levels
   (make the level count a parameter, and test at 32) -> back to sRGB for display.
   Optionally apply the 32-colour / lossy path so you see codec damage too.
3. Draw each pixel as a **round dot on black with gaps** (dot diameter ~60-70% of
   pitch), upscaled at least 12x. A plain nearest-neighbour upscale lies to you: it
   makes dither look coarser and thin lines look more solid than the real thing.
   A slight bloom on bright dots gets closer still.
4. Offer a "squint" view: the same thing blurred, to approximate viewing distance.

Judge every piece in that preview, at actual physical size on your screen if you
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
- Separate *piece* (a pure function of time, seed and parameters -> frame) from
  *output* (preview, pipe, sender). It makes pieces testable and lets one process
  render to the preview and the panel at once.
- Build in a global brightness/limiter stage at the end of your pipeline (average
  picture level cap, flash limiter from section 4). Pieces should not each have to
  remember the rules.
- When something looks wrong on the device but right in your preview, that is a bug
  in the preview's panel model or in our pipeline. Capture the frame bytes that
  produced it and report it; do not tune the art around it.
