//! Synthetic test content, one generator per content class in card 002.
//!
//! Everything except the text/UI mock is rendered at 4x resolution and box
//! filtered down, because that is what a real sender does (render or downscale
//! a large image) and it is what produces realistic anti-aliased detail and
//! high distinct-colour counts. Rendering directly at 64x32 would produce
//! artificially palette-friendly frames and flatter the palette codecs.
//!
//! The text/UI mock is drawn at 1x on purpose: class (b) content is
//! pixel-authored and must survive pixel-exact.

use crate::color::lin_to_srgb8;
use crate::font;
use crate::frame::{Clip, Frame, H, W};

pub const NFRAMES: usize = 60;
const SS: usize = 4; // supersample factor

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [
        (r * 255.0 + 0.5) as u8,
        (g * 255.0 + 0.5) as u8,
        (b * 255.0 + 0.5) as u8,
    ]
}

/// Render at SSxSS and box-filter down. The box filter averages in *linear
/// light*, which is the physically correct thing and matters at this size.
fn supersample<F: Fn(f32, f32) -> [u8; 3]>(f: F) -> Frame {
    let t = &*crate::color::SRGB_TO_LIN;
    let mut out = Frame::black();
    let n = (SS * SS) as f32;
    for y in 0..H {
        for x in 0..W {
            let mut acc = [0f32; 3];
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    let c = f(fx, fy);
                    for k in 0..3 {
                        acc[k] += t[c[k] as usize];
                    }
                }
            }
            out.set(
                x,
                y,
                [
                    lin_to_srgb8(acc[0] / n),
                    lin_to_srgb8(acc[1] / n),
                    lin_to_srgb8(acc[2] / n),
                ],
            );
        }
    }
    out
}

// --- (a) visualiser: plasma ------------------------------------------------

pub fn plasma() -> Clip {
    let frames = (0..NFRAMES)
        .map(|i| {
            let t = i as f32 * 0.09;
            supersample(|x, y| {
                let d = ((x - 32.0).powi(2) + (y - 16.0).powi(2)).sqrt();
                let v = (x / 7.0 + t).sin()
                    + (y / 5.0 - t * 0.7).sin()
                    + ((x + y) / 9.0 + t * 1.3).sin()
                    + (d / 5.0 - t * 2.0).sin();
                hsv(v / 8.0 + 0.5 + t * 0.05, 1.0, 1.0)
            })
        })
        .collect();
    Clip {
        name: "plasma",
        blurb: "smooth full-frame motion, fully saturated hues, no hard edges",
        frames,
    }
}

// --- (a) visualiser: mandelbrot zoom --------------------------------------

pub fn mandelbrot() -> Clip {
    // A point on the boundary with interesting structure at every scale.
    let (cx, cy) = (-0.743_643_9f64, 0.131_825_9f64);
    let frames = (0..NFRAMES)
        .map(|i| {
            let zoom = 1.6 * 0.90f64.powi(i as i32);
            let phase = i as f32 * 0.02;
            supersample(|px, py| {
                let x0 = cx + (px as f64 / W as f64 - 0.5) * zoom * 2.0;
                let y0 = cy + (py as f64 / H as f64 - 0.5) * zoom;
                let (mut zx, mut zy) = (0f64, 0f64);
                let maxit = 400;
                let mut it = 0;
                while zx * zx + zy * zy <= 512.0 && it < maxit {
                    let xt = zx * zx - zy * zy + x0;
                    zy = 2.0 * zx * zy + y0;
                    zx = xt;
                    it += 1;
                }
                if it >= maxit {
                    return [0, 0, 0];
                }
                // Smooth (continuous) iteration count.
                let log_zn = (zx * zx + zy * zy).ln() / 2.0;
                let nu = (log_zn / std::f64::consts::LN_2).ln() / std::f64::consts::LN_2;
                let s = (it as f64 + 1.0 - nu) as f32;
                hsv(s * 0.021 + phase, 0.95, (s * 0.09).min(1.0).powf(0.6))
            })
        })
        .collect();
    Clip {
        name: "mandel",
        blurb: "fractal zoom: fine high-contrast detail plus saturated colour cycling",
        frames,
    }
}

// --- (b) text / UI ---------------------------------------------------------

pub fn textui() -> Clip {
    const BG: [u8; 3] = [0, 0, 0];
    const PANEL: [u8; 3] = [16, 20, 48];
    const WHITE: [u8; 3] = [255, 255, 255];
    const AMBER: [u8; 3] = [255, 176, 0];
    const CYAN: [u8; 3] = [0, 224, 255];
    const GREEN: [u8; 3] = [0, 224, 96];
    const RED: [u8; 3] = [255, 48, 48];
    const MARQUEE: &str = "NOW PLAYING: Kraftwerk - Computer World   *   64x32 @ 30fps   *   ";

    let frames = (0..NFRAMES)
        .map(|i| {
            let mut f = Frame::black();
            for y in 0..H {
                for x in 0..W {
                    f.set(x, y, if (8..22).contains(&y) { PANEL } else { BG });
                }
            }
            // Header: icon block + title.
            for y in 1..7 {
                for x in 1..7 {
                    let on = (x + y) % 2 == 0 || x == 1 || x == 6;
                    f.set(x, y, if on { CYAN } else { BG });
                }
            }
            font::draw_text("SCREENY", 9, 1, |x, y| {
                if x >= 0 && (x as usize) < W && y >= 0 && (y as usize) < H {
                    f.set(x as usize, y as usize, WHITE);
                }
            });

            // Clock, advancing one second per two frames.
            let secs = 12 * 3600 + 34 * 60 + i / 2;
            let txt = format!("{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60);
            font::draw_text(&txt, 3, 10, |x, y| {
                if x >= 0 && (x as usize) < W && y >= 0 && (y as usize) < H {
                    f.set(x as usize, y as usize, AMBER);
                }
            });
            // Blinking colon: erase it on odd seconds.
            if (i / 2) % 2 == 1 {
                for y in 10..17 {
                    for x in 15..21 {
                        if f.get(x, y) == AMBER {
                            f.set(x, y, PANEL);
                        }
                    }
                }
            }

            // Progress bar.
            let frac = (i as f32 / NFRAMES as f32 * 1.7).min(1.0);
            let wfill = (frac * 26.0) as usize;
            for x in 0..28 {
                for y in 18..21 {
                    let c = if x == 0 || x == 27 || y == 18 || y == 20 {
                        WHITE
                    } else if x <= wfill {
                        GREEN
                    } else {
                        PANEL
                    };
                    f.set(34 + x, y, c);
                }
            }
            // Right-hand status glyphs.
            font::draw_text("WIFI", 36, 10, |x, y| {
                if x >= 0 && (x as usize) < W && y >= 0 && (y as usize) < H {
                    f.set(x as usize, y as usize, CYAN);
                }
            });
            for y in 11..16 {
                for x in 60..63 {
                    f.set(x, y, if y > 13 { GREEN } else { RED });
                }
            }

            // Bottom marquee, 1 px per frame.
            let tw = font::text_width(MARQUEE) as i32;
            let off = -(i as i32) % tw;
            for rep in 0..2 {
                font::draw_text(MARQUEE, off + rep * tw, 24, |x, y| {
                    if x >= 0 && (x as usize) < W && y >= 0 && (y as usize) < H {
                        f.set(x as usize, y as usize, WHITE);
                    }
                });
            }
            f
        })
        .collect();
    Clip {
        name: "textui",
        blurb: "pixel-authored UI: 8 colours, hard edges, mostly static, scrolling marquee",
        frames,
    }
}

// --- (c) photo-like stress case -------------------------------------------

fn hash2(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd8163841) ^ seed;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

fn vnoise(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (x - xi, y - yi);
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (i, j) = (xi as i32, yi as i32);
    let a = hash2(i, j, seed);
    let b = hash2(i + 1, j, seed);
    let c = hash2(i, j + 1, seed);
    let d = hash2(i + 1, j + 1, seed);
    let top = a + (b - a) * sx;
    let bot = c + (d - c) * sx;
    top + (bot - top) * sy
}

fn fbm(x: f32, y: f32, seed: u32, oct: u32) -> f32 {
    let mut v = 0.0;
    let mut amp = 0.5;
    let mut fx = 1.0;
    for o in 0..oct {
        v += amp * vnoise(x * fx, y * fx, seed + o * 977);
        amp *= 0.5;
        fx *= 2.0;
    }
    v
}

pub fn photo() -> Clip {
    let frames = (0..NFRAMES)
        .map(|i| {
            let pan = i as f32 * 0.35; // slow horizontal pan
            supersample(|x, y| {
                let wx = x + pan;
                // Sky: warm low sun through to deep blue overhead.
                let t = (y / 22.0).clamp(0.0, 1.0);
                let mut r = 250.0 * (1.0 - t) + 24.0 * t;
                let mut g = 150.0 * (1.0 - t) + 60.0 * t;
                let mut b = 60.0 * (1.0 - t) + 170.0 * t;
                // Sun disc with glow.
                let sd = ((wx - 46.0).powi(2) + (y - 13.0).powi(2) * 1.4).sqrt();
                let glow = (-(sd / 9.0)).exp();
                r += 240.0 * glow;
                g += 190.0 * glow;
                b += 90.0 * glow;
                if sd < 3.4 {
                    r = 255.0;
                    g = 248.0;
                    b = 210.0;
                }
                // Clouds.
                let cl = (fbm(wx * 0.09, y * 0.16, 11, 4) - 0.42).max(0.0) * 2.4;
                let cl = cl * (1.0 - t * 0.6);
                r = r * (1.0 - cl) + 255.0 * cl;
                g = g * (1.0 - cl) + 205.0 * cl;
                b = b * (1.0 - cl) + 190.0 * cl;
                // Ridge line and textured foreground.
                let ridge = 21.0 + 3.0 * (wx * 0.11).sin() + 2.5 * fbm(wx * 0.07, 0.0, 7, 3);
                if y > ridge {
                    let depth = ((y - ridge) / 11.0).clamp(0.0, 1.0);
                    let tex = fbm(wx * 0.55, y * 0.55, 23, 4);
                    let blade = fbm(wx * 2.1, y * 1.3, 31, 2);
                    r = 22.0 + 60.0 * depth + 70.0 * tex * depth + 30.0 * blade * depth;
                    g = 34.0 + 90.0 * depth + 95.0 * tex * depth + 40.0 * blade * depth;
                    b = 26.0 + 30.0 * depth + 40.0 * tex * depth + 20.0 * blade * depth;
                }
                [
                    r.clamp(0.0, 255.0) as u8,
                    g.clamp(0.0, 255.0) as u8,
                    b.clamp(0.0, 255.0) as u8,
                ]
            })
        })
        .collect();
    Clip {
        name: "photo",
        blurb: "photo-like stress case: broadband detail, texture, sun highlight, slow pan",
        frames,
    }
}

// --- dark-end probe --------------------------------------------------------

pub fn darkfade() -> Clip {
    let frames = (0..NFRAMES)
        .map(|i| {
            let k = i as f32 / (NFRAMES - 1) as f32;
            let level = 0.02 + 0.22 * (1.0 - k); // stays in the bottom quarter
            supersample(|x, y| {
                let g = (1.0 - (y / H as f32)) * level;
                let n = fbm(x * 0.13 + 3.0, y * 0.13, 5, 3);
                let mut r = g * 90.0 + n * 14.0 * level * 4.0;
                let mut gg = g * 60.0 + n * 10.0 * level * 4.0;
                let mut b = g * 200.0 + n * 26.0 * level * 4.0;
                // A handful of bright stars: the contrast case that makes
                // dark-end quantisation obvious.
                let sx = (x * 0.5).floor() as i32;
                let sy = (y * 0.5).floor() as i32;
                if hash2(sx, sy, 99) > 0.985 {
                    let s = 140.0 + 115.0 * hash2(sx, sy, 100);
                    r = s;
                    gg = s;
                    b = s;
                }
                [
                    r.clamp(0.0, 255.0) as u8,
                    gg.clamp(0.0, 255.0) as u8,
                    b.clamp(0.0, 255.0) as u8,
                ]
            })
        })
        .collect();
    Clip {
        name: "darkfade",
        blurb: "near-black gradient fading further down, with a few bright stars",
        frames,
    }
}

pub fn all() -> Vec<Clip> {
    vec![plasma(), mandelbrot(), textui(), photo(), darkfade()]
}
