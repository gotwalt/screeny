#!/bin/bash
# Render an "LED photograph" GIF + PNG for each screeny-art patch, for the
# public README gallery. Offline only: screeny-art pipe | ffmpeg. No panel,
# no network, no serial.
#
# Usage: tools/render-media.sh [patch ...]   (default: all patches from
# `screeny-art list`, in the order below). Writes docs/media/<patch>.gif,
# docs/media/<patch>.png, and docs/media/gallery.png (only when all eight
# patches' stills are present after this run).
set -euo pipefail
cd "$(dirname "$0")/.."

FF=/opt/homebrew/bin/ffmpeg
ART=target/release/screeny-art
[ -x "$FF" ] || { echo "render-media: no ffmpeg at $FF" >&2; exit 1; }
if [ ! -x "$ART" ]; then
  echo "render-media: building screeny-art --release (the debug encoder is ~10x slower)" >&2
  cargo build --release -p screeny-art
fi

OUT=docs/media
mkdir -p "$OUT"
TMP="target/media-tmp/$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

# Patch order + how each is captured. `pipe` writes WireFrame.rgb: the panel's
# own RGB after the limiter and after quantisation to its duty steps
# (Panel::snap8, pipeline.rs) - post-quantisation, i.e. what would go out to
# the panel, but before the sender's lossy codec round-trip (that "decoded
# datagram" view is `preview`, which only `snapshot`/the studio expose - pipe
# has no flag for it). This is the closer-to-honest of the two pipe can give.
# `--time` pins the clock so a minute change falls inside the clip; `--seed`
# is fixed per patch for a reproducible render.
ALL_PATCHES="clocks-numerals clocks-dials vesta metaballs flock overland lattice knot"
PATCHES="${*:-$ALL_PATCHES}"

args_for() {
  case "$1" in
    clocks-numerals) echo "--seed 7 --time 21:11:56 --seconds 8" ;;
    clocks-dials)    echo "--seed 7 --time 21:11:56 --seconds 8" ;;
    vesta)           echo "--time 09:59:56 --seconds 8" ;; # no --seed: vesta has no randomness
    metaballs)       echo "--seed 3 --seconds 6" ;;
    flock)           echo "--seed 11 --seconds 6" ;;
    overland)        echo "--seed 1 --seconds 6" ;;
    lattice)         echo "--seed 2 --seconds 6" ;;
    knot)            echo "--seed 4 --seconds 6" ;;
    *) echo "render-media: unknown patch \`$1\`" >&2; exit 1 ;;
  esac
}

# One 8x8 tile, tiled 64x32 -> scaled 512x256, a soft round dot (Gaussian
# falloff, radius ~1.5 px of 4) on black: this is the "LED, not a smooth
# picture" mask everything below multiplies against.
MASK="$TMP/mask.png"
"$FF" -y -f lavfi -i nullsrc=size=512x256 -vf \
  "geq=lum='255*exp(-0.5*pow(hypot(mod(X,8)-3.5,mod(Y,8)-3.5)/1.5,2))':cb=128:cr=128" \
  -frames:v 1 "$MASK" >/dev/null 2>&1

# The shared look: nearest-neighbour 8x upscale (each LED becomes a crisp 8x8
# block, no smoothing), multiplied by the dot mask (dark gaps between LEDs),
# then a soft screen-blended blur copy for a little photographic glow.
# `fps` (15 or 30) is a filter step so the palette/still stages see it too.
led_look() {
  local fps="$1"
  echo "[0:v]scale=512:256:flags=neighbor,format=rgb24[big];\
[1:v]format=rgb24[msk];\
[big][msk]blend=all_mode=multiply:all_opacity=1[dotted];\
[dotted]split=2[base][glowsrc];[glowsrc]gblur=sigma=4[glow];\
[base][glow]blend=all_mode=screen:all_opacity=0.55,fps=$fps[led]"
}

render_one() {
  local id="$1"
  local raw="$TMP/$id.raw" gif="$OUT/$id.gif" png="$OUT/$id.png"
  local args frames dur fps=30 bytes
  args=$(args_for "$id")
  echo "== $id ($args) ==" >&2
  # shellcheck disable=SC2086
  "$ART" pipe "$id" $args > "$raw"
  bytes=$(wc -c < "$raw"); frames=$((bytes / 6144))
  dur=$(awk -v f="$frames" 'BEGIN{printf "%.4f", f/30}')

  for fps in 30 15; do
    # `-t "$dur"` on the looped mask bounds it to the clip length: without it
    # blend's default eof_action=repeat lets the infinite loop input run
    # forever once the finite raw stream ends (found the hard way - it will
    # happily render hours of frames into /dev/null).
    "$FF" -y -f rawvideo -pix_fmt rgb24 -s 64x32 -r 30 -i "$raw" \
      -loop 1 -framerate 30 -t "$dur" -i "$MASK" \
      -filter_complex "$(led_look "$fps");[led]split=2[vid][palin];[palin]palettegen=stats_mode=diff[pal];[vid][pal]paletteuse=dither=bayer:bayer_scale=3[out]" \
      -map "[out]" "$gif" >/dev/null 2>"$TMP/$id.err"
    [ "$(wc -c < "$gif")" -le 3145728 ] && break
    echo "  $id: over 3 MB at ${fps}fps, dropping to 15 fps" >&2
  done

  # The still: one frame (near the minute turn for the clocks/vesta), same
  # look, full colour (no GIF palette).
  local mid=$((frames / 2))
  "$FF" -y -f rawvideo -pix_fmt rgb24 -s 64x32 -r 30 -i "$raw" \
    -loop 1 -framerate 30 -t "$dur" -i "$MASK" \
    -filter_complex "$(led_look 30);[led]select=eq(n\,$mid)[out]" \
    -map "[out]" -frames:v 1 "$png" >/dev/null 2>>"$TMP/$id.err"

  printf "  %-16s gif %8d bytes   png %7d bytes\n" "$id" "$(wc -c < "$gif")" "$(wc -c < "$png")"
}

for p in $PATCHES; do render_one "$p"; done

# Gallery: only once every patch's still actually exists on disk.
have_all=1
for p in $ALL_PATCHES; do [ -f "$OUT/$p.png" ] || have_all=0; done
if [ "$have_all" = 1 ]; then
  inputs=(); filt=""
  i=0
  for p in $ALL_PATCHES; do inputs+=(-i "$OUT/$p.png"); filt+="[$i:v]pad=512+16:256+16:8:8:black[p$i];"; i=$((i+1)); done
  filt+="[p0][p1][p2][p3]hstack=4[row0];[p4][p5][p6][p7]hstack=4[row1];[row0][row1]vstack=2[out]"
  "$FF" -y "${inputs[@]}" -filter_complex "$filt" -map "[out]" "$OUT/gallery.png" >/dev/null 2>"$TMP/gallery.err"
  echo "gallery.png: $(wc -c < "$OUT/gallery.png") bytes"
else
  echo "gallery.png skipped: not all eight patches were rendered this run" >&2
fi
