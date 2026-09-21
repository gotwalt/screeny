#!/usr/bin/env python3
"""Generate `crates/art/src/faces/data.rs` - the art crate's numeral faces.

`vesta` draws `HH:MM` on four 14 x 30 LED modules, and card 174 made the
numerals' face a choice. Everything but the house face (`Vesta`, stroked paths
in `crates/art/src/faces/stroked.rs`) is a bitmap baked into the source here,
so the crate has no font dependency at run time and the picture is
reproducible from two upstream repositories:

    https://github.com/Tecate/bitmap-fonts     (the BDFs)
    https://github.com/eliheuer/micro-grotesk  (one variable TTF)

Usage:

    python3 tools/art-faces.py \
        --bitmap-fonts ~/src/bitmap-fonts \
        --micro-grotesk ~/src/micro-grotesk \
        --out crates/art/src/faces/data.rs

`--preview` prints every face as ASCII instead of writing anything, which is
how the faces were chosen. Rasterising Micro Grotesk needs Pillow with
FreeType; the BDF faces need nothing but the standard library.

Only `'0'..='9'` are cut today, but a face's glyphs are listed by `char`, so
adding letters is a change here and in `GLYPHS` rather than in any caller.

**Licences are part of the data.** Only faces whose upstream licence allows
redistribution are listed in `FACES`, and each one is written up in
`crates/art/src/faces/FACES.md`. Read a BDF's own COPYRIGHT/NOTICE properties
before adding one.
"""

import argparse
import math
import os
import subprocess
import sys

# --------------------------------------------------------------- the faces

# The glyphs a face may carry, in the order the data holds them. The **box is
# the digits'**: every glyph is sliced out of the font's own cell with the box
# the ten numerals share, so a colon keeps the position, weight and baseline
# the font gave it relative to them, and a patch that has placed the digits has
# placed the colon too. A face whose font has not got one of these does not
# carry it, and a face that does not carry a glyph draws nothing for it
# (card 175).
GLYPHS = "0123456789:"
# The glyphs that set the box. Everything else is placed against them.
BOXED_BY = "0123456789"

# `scale` is LEDs per font pixel. A module is 14 x 30 LEDs and the numerals
# sit in about 13 x 22 of it, so a 1:1 face wants digits near 12-13 x 20 and a
# x2 face wants them near 6 x 9.
FACES = [
    dict(
        name="Vesta",
        kind="stroked",
        source="crates/art/src/faces/stroked.rs",
        # Ten numerals and nothing else: the colon vesta draws beside them is
        # the patch's own pair of dots, not a glyph.
        glyphs="0123456789",
        note="the house face: stroked paths, so `weight` moves its stroke",
    ),
    dict(
        name="Terminus Bold",
        kind="bdf",
        file="bitmap/terminus-font-4.39/ter-u32b.bdf",
        repo="bitmap-fonts",
        scale=1,
        note="13 x 20 at 1:1, 3-LED strokes",
    ),
    dict(
        name="Terminus",
        kind="bdf",
        file="bitmap/terminus-font-4.39/ter-u32n.bdf",
        repo="bitmap-fonts",
        scale=1,
        note="12 x 20 at 1:1, 2-LED strokes",
    ),
    dict(
        name="Spleen",
        kind="bdf",
        file="bitmap/spleen/spleen-16x32.bdf",
        repo="bitmap-fonts",
        scale=1,
        note="12 x 20 at 1:1, 2-LED strokes, squarer",
    ),
    dict(
        name="Dina",
        kind="bdf",
        file="bitmap/dina/Dina_r400-10.bdf",
        repo="bitmap-fonts",
        scale=2,
        note="6 x 9 at x2: 2 x 2-LED pixels",
    ),
    dict(
        name="Micro Grotesk",
        kind="ttf",
        file="fonts/MicroGrotesk[wght].ttf",
        repo="micro-grotesk",
        weight=400,
        height=22,
        condense=0.80,
        samples=4,
        # Micro Grotesk has no colon. Rasterising one gives the .notdef box,
        # which is a rectangle a quarter taller than the digits, and there is
        # no way to ask a TTF for its cmap without fontTools - so it is
        # declared here rather than guessed at. vesta draws its own dots for a
        # face with no `:`.
        glyphs="0123456789",
        note="an outline face, 4 samples per LED; no colon of its own",
    ),
]

DEFAULT = "Terminus Bold"

REPOS = {
    "bitmap-fonts": "github.com/Tecate/bitmap-fonts",
    "micro-grotesk": "github.com/eliheuer/micro-grotesk",
}

# --------------------------------------------------------------- BDF digits


def bdf_glyphs(path, wanted):
    """`wanted`'s glyphs as rows of 0/1, every one sliced with the digits' box."""
    cells = {}
    fbb = None
    enc = None
    bbx = None
    lines = open(path, errors="replace").read().splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith("FONTBOUNDINGBOX"):
            fbb = list(map(int, line.split()[1:5]))
        elif line.startswith("ENCODING"):
            enc = int(line.split()[1])
        elif line.startswith("BBX"):
            bbx = list(map(int, line.split()[1:5]))
        elif line == "BITMAP":
            rows = []
            i += 1
            while lines[i] != "ENDCHAR":
                rows.append(lines[i])
                i += 1
            ch = chr(enc) if 0 <= enc < 0x110000 else None
            if ch in wanted and ch not in cells:
                gw, gh, xo, yo = bbx
                fw, fh, fx, fy = fbb
                cell = [[0] * fw for _ in range(fh)]
                for r, hexrow in enumerate(rows):
                    bits = bin(int(hexrow, 16))[2:].zfill(len(hexrow) * 4)
                    y = (fh + fy) - (yo + gh) + r
                    for x in range(gw):
                        if bits[x] == "1" and 0 <= y < fh and 0 <= x + xo - fx < fw:
                            cell[y][x + xo - fx] = 1
                cells[ch] = cell
        i += 1
    missing = [c for c in wanted if c not in cells]
    if missing:
        raise SystemExit("%s: has no %s" % (path, ", ".join(repr(c) for c in missing)))
    box = [cells[c] for c in BOXED_BY]
    ys = [y for c in box for y, row in enumerate(c) if any(row)]
    xs = [x for c in box for row in c for x, v in enumerate(row) if v]
    y0, y1, x0, x1 = min(ys), max(ys), min(xs), max(xs)

    def slice_(ch):
        cell = cells[ch]
        lost = sum(v for y, row in enumerate(cell) for x, v in enumerate(row) if v and not (y0 <= y <= y1 and x0 <= x <= x1))
        if lost:
            raise SystemExit("%s: %r has %d cells of ink outside the digits' box" % (path, ch, lost))
        return [row[x0 : x1 + 1] for row in cell[y0 : y1 + 1]]

    return [slice_(c) for c in wanted]


# ------------------------------------------------------- Micro Grotesk mask


def ttf_glyphs(path, weight, height, condense, samples, wanted):
    """`wanted`'s glyphs of a variable TTF as a `samples`-per-LED coverage mask.

    The box is the digits' own cap box (the `0`'s, so every digit shares a
    baseline), `height` LEDs tall; `condense` squeezes it horizontally, which
    is how a face wider than the module is made to fit.
    """
    from PIL import Image, ImageDraw, ImageFont

    rows = height * samples

    def draw(size, ch):
        font = ImageFont.truetype(path, size)
        font.set_variation_by_axes([weight])
        img = Image.new("L", (size * 3, size * 3), 0)
        ImageDraw.Draw(img).text((size, size), ch, fill=255, font=font)
        return img

    probe = draw(400, "0").getbbox()
    size = int(round(400 * rows / (probe[3] - probe[1])))
    zero = draw(size, "0").getbbox()
    out = []
    widths = []
    glyphs = []
    for d in wanted:
        img = draw(size, d)
        box = img.getbbox()
        g = img.crop((box[0], zero[1], box[2], zero[3]))
        if condense != 1.0:
            g = g.resize((max(1, int(round(g.width * condense))), g.height), Image.LANCZOS)
        glyphs.append(g)
        widths.append(g.width)
    cols = max(widths)
    for g in glyphs:
        pad = (cols - g.width) // 2
        bits = []
        for y in range(rows):
            row = [0] * cols
            for x in range(g.width):
                if g.getpixel((x, min(y, g.height - 1))) >= 128:
                    row[x + pad] = 1
            bits.append(row)
        out.append(bits)
    return out


# --------------------------------------------------------------- packing


def pack(face, digits):
    """A face's digits as (w, h, x0, y0, rows[]) in LEDs and bits."""
    h = len(digits[0])
    w = max(len(r) for g in digits for r in g)
    scale = face["scale"] if face["kind"] == "bdf" else 1.0 / face["samples"]
    if w > 64:
        raise SystemExit("%s: %d cells across, more than a u64 row" % (face["name"], w))
    bw, bh = w * scale, h * scale
    # The box's corner, in LEDs from the glyph's centre, snapped to a whole LED
    # so a whole-LED-per-cell face lands 1:1 on the panel: module centres and
    # the axle are both pixel boundaries, so an odd-LED-wide face has to sit
    # half an LED off centre or every one of its columns would straddle two.
    x0 = -math.floor(bw / 2 + 0.5)
    y0 = -math.floor(bh / 2 + 0.5)
    rows = []
    for g in digits:
        for y in range(h):
            bits = 0
            row = g[y] if y < len(g) else []
            for x, v in enumerate(row):
                if v:
                    bits |= 1 << x
            rows.append(bits)
    crisp = float(scale).is_integer()
    return dict(w=w, h=h, scale=scale, x0=x0, y0=y0, crisp=crisp, rows=rows, bw=bw, bh=bh)


def ident(name):
    return "".join(c if c.isalnum() else "_" for c in name).upper()


def glyph_ident(chars):
    """A name for a glyph set: what it has beyond the digits, or `DIGITS`."""
    extra = "".join(c for c in chars if c not in BOXED_BY)
    names = {":": "COLON"}
    return "DIGITS" if not extra else "DIGITS_" + "_".join(names.get(c, "U%04X" % ord(c)) for c in extra)


def revision(root):
    try:
        out = subprocess.run(
            ["git", "-C", root, "rev-parse", "--short=10", "HEAD"],
            capture_output=True,
            text=True,
            timeout=20,
        )
        if out.returncode == 0:
            return out.stdout.strip()
    except Exception:
        pass
    return "unknown"


# --------------------------------------------------------------- output


def preview(face, packed, digits):
    print(
        "== %s  %d x %d cells, x%g -> %g x %g LEDs, box at (%d, %d)%s"
        % (
            face["name"],
            packed["w"],
            packed["h"],
            packed["scale"],
            packed["bw"],
            packed["bh"],
            packed["x0"],
            packed["y0"],
            "  CRISP" if packed["crisp"] else "",
        )
    )
    for y in range(packed["h"]):
        line = []
        for g in digits:
            row = g[y] if y < len(g) else []
            line.append("".join("#" if x < len(row) and row[x] else "." for x in range(packed["w"])))
        print("  " + "  ".join(line))
    n = len(packed["rows"]) // packed["h"]
    ink = sum(bin(r).count("1") for r in packed["rows"]) / n
    print("  ink: %.1f cells a glyph, %.1f LEDs" % (ink, ink * packed["scale"] ** 2))


def emit(faces, revs, out):
    names = [f["name"] for f in faces]
    if DEFAULT not in names:
        raise SystemExit("the default face %r is not in the list" % DEFAULT)
    w = []
    w.append("//! The faces themselves - **generated** by `tools/art-faces.py`, do not edit.")
    w.append("//!")
    w.append("//! Card 174: the owner did not like the one face vesta had, so the face is a")
    w.append("//! choice. A bitmap face's cells *are* LEDs - drawn at 1:1 every one of them")
    w.append("//! lands whole on one LED, which is a kind of crisp a stroked face cannot be")
    w.append("//! on a 64 x 32 panel; see the half-LED rule in the module above.")
    w.append("//! `crates/art/src/faces/FACES.md` says where each face came from and under")
    w.append("//! what licence; the bitmaps are unmodified extracts of the digits.")
    w.append("//!")
    w.append("//! Generated from:")
    w.append("//!")
    for key, slug in sorted(REPOS.items()):
        w.append("//! - `%s` at `%s`" % (slug, revs.get(key, "unknown")))
    w.append("")
    w.append("use super::{Bitmap, Face, Ink};")
    w.append("")
    for chars in sorted({f.get("glyphs", GLYPHS) for f in faces if f["kind"] != "stroked"} | {GLYPHS}):
        w.append("/// `%s`." % chars)
        w.append("const %s: &[char] = &[%s];" % (glyph_ident(chars), ", ".join("'%s'" % c for c in chars)))
        w.append("")
    w.pop()
    w.append("")
    w.append("/// The named stops of a `font` parameter, in order. Hand this straight to")
    w.append("/// `choice(..)`: `choice(\"font\", \"Numerals\", faces::NAMES, faces::DEFAULT)`.")
    w.append("pub const NAMES: &[&str] = &[")
    for n in names:
        w.append('    "%s",' % n)
    w.append("];")
    w.append("")
    w.append("/// Which face a patch starts on.")
    w.append("pub const DEFAULT: f32 = %d.0;" % names.index(DEFAULT))
    w.append("")
    w.append("/// Every face, in the same order as [`NAMES`].")
    w.append("pub static FACES: &[Face] = &[")
    for f in faces:
        w.append("    Face {")
        w.append('        name: "%s",' % f["name"])
        w.append('        source: "%s",' % f["source"])
        w.append('        note: "%s",' % f["note"])
        w.append("        glyphs: %s," % glyph_ident(f.get("glyphs", GLYPHS)))
        if f["kind"] == "stroked":
            w.append("        ink: Ink::Stroked,")
        else:
            p = f["packed"]
            w.append("        ink: Ink::Bits(Bitmap {")
            w.append("            w: %d," % p["w"])
            w.append("            h: %d," % p["h"])
            w.append("            scale: %s," % ("%g" % p["scale"] if not float(p["scale"]).is_integer() else "%.1f" % p["scale"]))
            w.append("            x0: %d.0," % p["x0"])
            w.append("            y0: %d.0," % p["y0"])
            w.append("            crisp: %s," % ("true" if p["crisp"] else "false"))
            w.append("            rows: &%s," % ident(f["name"]))
            w.append("        }),")
        w.append("    },")
    w.append("];")
    w.append("")
    for f in faces:
        if f["kind"] == "stroked":
            continue
        p = f["packed"]
        w.append("/// `%s`: %d rows a glyph, bit `x` of a row is cell `x` from the left." % (f["name"], p["h"]))
        w.append("#[rustfmt::skip]")
        w.append("static %s: [u64; %d] = [" % (ident(f["name"]), len(p["rows"])))
        digits_wide = 4 if p["w"] <= 16 else 16
        for d, ch in enumerate(f.get("glyphs", GLYPHS)):
            w.append("    // %s" % ch)
            rows = p["rows"][d * p["h"] : (d + 1) * p["h"]]
            per = max(1, 64 // (digits_wide + 4))
            for i in range(0, len(rows), per):
                w.append("    " + " ".join("0x%0*x," % (digits_wide, r) for r in rows[i : i + per]))
        w.append("];")
        w.append("")
    open(out, "w").write("\n".join(w) + "\n")


# --------------------------------------------------------------- main


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bitmap-fonts", required=True, help="a clone of Tecate/bitmap-fonts")
    ap.add_argument("--micro-grotesk", required=True, help="a clone of eliheuer/micro-grotesk")
    ap.add_argument("--out", help="where to write faces.rs")
    ap.add_argument("--preview", action="store_true", help="print the faces as ASCII and write nothing")
    args = ap.parse_args()

    roots = {"bitmap-fonts": args.bitmap_fonts, "micro-grotesk": args.micro_grotesk}
    revs = {k: revision(v) for k, v in roots.items()}

    for face in FACES:
        if face["kind"] == "stroked":
            continue
        path = os.path.join(roots[face["repo"]], face["file"])
        if not os.path.exists(path):
            raise SystemExit("missing: %s" % path)
        if face["kind"] == "bdf":
            glyphs = bdf_glyphs(path, face.get("glyphs", GLYPHS))
        else:
            glyphs = ttf_glyphs(path, face["weight"], face["height"], face["condense"], face["samples"], face.get("glyphs", GLYPHS))
        face["packed"] = pack(face, glyphs)
        face["source"] = "%s %s %s" % (REPOS[face["repo"]], revs[face["repo"]], face["file"])
        if args.preview:
            preview(face, face["packed"], glyphs)
            print()

    if args.preview:
        return
    if not args.out:
        raise SystemExit("--out is required without --preview")
    emit(FACES, revs, args.out)
    print("wrote %s: %d faces" % (args.out, len(FACES)))


if __name__ == "__main__":
    sys.exit(main())
