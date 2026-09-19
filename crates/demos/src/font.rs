//! A hand-made bitmap font. Upper case only, 7 rows tall, **proportional**:
//! most letters are 5 wide, E/F/J/L are 4, I is 3, the apostrophe is 1.
//!
//! Proportional is not a flourish, it is what makes the word clock fit. The
//! longest phrase word pair, "TWENTY FIVE", is 55 px proportional and 65 px on
//! a 5-px monospace grid - which does not fit on a 64 px panel at all.
//!
//! Outline fonts are never rasterised at this size (brief section 3); every
//! glyph below was drawn by hand as a pixel grid.

use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Copy)]
pub struct Glyph {
    pub w: usize,
    /// One bit per column, MSB = leftmost column of the glyph.
    pub rows: [u8; 7],
}

impl Glyph {
    #[inline]
    pub fn on(&self, x: usize, y: usize) -> bool {
        if x >= self.w || y >= 7 {
            return false;
        }
        self.rows[y] & (1 << (self.w - 1 - x)) != 0
    }
}

/// Gap between glyphs within a word, in pixels.
pub const TRACK: usize = 1;
/// Gap between words, in pixels.
pub const WORD_GAP: usize = 3;
pub const ROWS: usize = 7;

const ART: &[(char, [&str; 7])] = &[
    (
        'A',
        [
            ".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
    ),
    (
        'B',
        [
            "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
        ],
    ),
    (
        'C',
        [
            ".###.", "#...#", "#....", "#....", "#....", "#...#", ".###.",
        ],
    ),
    (
        'D',
        [
            "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.",
        ],
    ),
    (
        'E',
        ["####", "#...", "#...", "###.", "#...", "#...", "####"],
    ),
    (
        'F',
        ["####", "#...", "#...", "###.", "#...", "#...", "#..."],
    ),
    (
        'G',
        [
            ".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".###.",
        ],
    ),
    (
        'H',
        [
            "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
    ),
    ('I', ["###", ".#.", ".#.", ".#.", ".#.", ".#.", "###"]),
    (
        'J',
        ["..##", "...#", "...#", "...#", "...#", "#..#", ".##."],
    ),
    (
        'K',
        [
            "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#",
        ],
    ),
    (
        'L',
        ["#...", "#...", "#...", "#...", "#...", "#...", "####"],
    ),
    (
        'M',
        [
            "#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#",
        ],
    ),
    (
        'N',
        [
            "#...#", "##..#", "#.#.#", "#.#.#", "#..##", "#...#", "#...#",
        ],
    ),
    (
        'O',
        [
            ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
    ),
    (
        'P',
        [
            "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
        ],
    ),
    (
        'Q',
        [
            ".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#",
        ],
    ),
    (
        'R',
        [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
    ),
    (
        'S',
        [
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ],
    ),
    (
        'T',
        [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
        ],
    ),
    (
        'U',
        [
            "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
    ),
    (
        'V',
        [
            "#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#..",
        ],
    ),
    (
        'W',
        [
            "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#",
        ],
    ),
    (
        'X',
        [
            "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
        ],
    ),
    (
        'Y',
        [
            "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
        ],
    ),
    (
        'Z',
        [
            "#####", "....#", "...#.", "..#..", ".#...", "#....", "#####",
        ],
    ),
    ('\'', ["#", "#", ".", ".", ".", ".", "."]),
    ('.', [".", ".", ".", ".", ".", ".", "#"]),
    (':', [".", ".", "#", ".", ".", "#", "."]),
    (
        '-',
        ["....", "....", "....", "####", "....", "....", "...."],
    ),
    (
        '/',
        [
            "....#", "....#", "...#.", "..#..", ".#...", "#....", "#....",
        ],
    ),
    (
        '+',
        [
            ".....", "..#..", "..#..", "#####", "..#..", "..#..", ".....",
        ],
    ),
    (
        '%',
        [
            "#...#", "#..#.", "...#.", "..#..", ".#...", ".#..#", "#...#",
        ],
    ),
    ('(', [".#", "#.", "#.", "#.", "#.", "#.", ".#"]),
    (')', ["#.", ".#", ".#", ".#", ".#", ".#", "#."]),
    (
        '0',
        [
            ".###.", "#..##", "#.#.#", "#.#.#", "##..#", "#...#", ".###.",
        ],
    ),
    ('1', ["..#", ".##", "..#", "..#", "..#", "..#", ".##"]),
    (
        '2',
        [
            ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
        ],
    ),
    (
        '3',
        [
            "####.", "....#", "....#", ".###.", "....#", "....#", "####.",
        ],
    ),
    (
        '4',
        [
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ],
    ),
    (
        '5',
        [
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
        ],
    ),
    (
        '6',
        [
            ".###.", "#...#", "#....", "####.", "#...#", "#...#", ".###.",
        ],
    ),
    (
        '7',
        [
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ],
    ),
    (
        '8',
        [
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ],
    ),
    (
        '9',
        [
            ".###.", "#...#", "#...#", ".####", "....#", "#...#", ".###.",
        ],
    ),
];

static GLYPHS: LazyLock<HashMap<char, Glyph>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for (ch, art) in ART {
        let w = art[0].len();
        let mut rows = [0u8; 7];
        for (y, r) in art.iter().enumerate() {
            assert_eq!(r.len(), w, "glyph {ch} row {y} is ragged");
            for (x, c) in r.chars().enumerate() {
                if c == '#' {
                    rows[y] |= 1 << (w - 1 - x);
                }
            }
        }
        m.insert(*ch, Glyph { w, rows });
    }
    m
});

pub fn glyph(c: char) -> Option<Glyph> {
    GLYPHS.get(&c.to_ascii_uppercase()).copied()
}

/// Width in pixels of a single word (no spaces expected).
pub fn word_width(s: &str) -> usize {
    let mut w = 0;
    let mut first = true;
    for c in s.chars() {
        let Some(g) = glyph(c) else { continue };
        if !first {
            w += TRACK;
        }
        w += g.w;
        first = false;
    }
    w
}

/// Width of a whole string, spaces included.
pub fn text_width(s: &str) -> usize {
    let mut w = 0;
    let mut first = true;
    for part in s.split(' ') {
        if part.is_empty() {
            continue;
        }
        if !first {
            w += WORD_GAP;
        }
        w += word_width(part);
        first = false;
    }
    w
}

/// Call `plot(x, y)` for every lit pixel of `s` drawn with its top-left at
/// (ox, oy). Coordinates may be negative; the caller clips.
pub fn draw(s: &str, ox: i32, oy: i32, mut plot: impl FnMut(i32, i32)) {
    let mut x = ox;
    let mut prev_space = true;
    for c in s.chars() {
        if c == ' ' {
            x += WORD_GAP as i32;
            prev_space = true;
            continue;
        }
        let Some(g) = glyph(c) else { continue };
        if !prev_space {
            x += TRACK as i32;
        }
        for row in 0..ROWS {
            for col in 0..g.w {
                if g.on(col, row) {
                    plot(x + col as i32, oy + row as i32);
                }
            }
        }
        x += g.w as i32;
        prev_space = false;
    }
}
