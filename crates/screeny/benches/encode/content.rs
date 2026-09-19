//! Synthetic benchmark content, one generator per content class in card 002.
//!
//! Everything except the UI mock is rendered at 4x and box-filtered down in
//! **linear light**, because that is what a real sender does and it is what
//! produces realistic anti-aliased detail and high distinct-colour counts.
//! Rendering directly at 64x32 would produce artificially palette-friendly
//! frames and flatter the palette codecs. The UI mock is drawn at 1x on
//! purpose: pixel-authored content must survive pixel-exact.

use screeny::color::{lin_to_srgb8, SRGB_TO_LIN};
use screeny::proto::{H, W};
use screeny::Frame;

use super::Clip;

/// Frames per clip.
pub const NFRAMES: usize = 30;
const SS: usize = 4;

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

fn supersample<F: Fn(f32, f32) -> [u8; 3]>(f: F) -> Frame {
    let t = &*SRGB_TO_LIN;
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

/// Smooth animated colour field: the many-colours, low-detail case.
fn plasma() -> Clip {
    let frames = (0..NFRAMES)
        .map(|i| {
            let t = i as f32 * 0.08;
            supersample(|x, y| {
                let v = (x * 0.13 + t).sin()
                    + (y * 0.17 - t * 0.7).sin()
                    + ((x * x + y * y).sqrt() * 0.11 + t * 1.3).sin();
                hsv(v * 0.17 + t * 0.05, 0.85, 0.45 + 0.35 * (v * 0.5).cos())
            })
        })
        .collect();
    Clip {
        name: "plasma",
        frames,
    }
}

/// Zooming fractal: fine detail everywhere, the hardest case for block codecs.
fn mandelbrot() -> Clip {
    let frames = (0..NFRAMES)
        .map(|i| {
            let zoom = 1.6 * 0.94f32.powi(i as i32);
            let (cx, cy) = (-0.743_643_9f32, 0.131_825_9f32);
            supersample(|px, py| {
                let x0 = cx + (px / W as f32 - 0.5) * zoom * 2.0;
                let y0 = cy + (py / H as f32 - 0.5) * zoom;
                let (mut x, mut y) = (0f32, 0f32);
                let mut n = 0u32;
                while x * x + y * y <= 4.0 && n < 200 {
                    let xt = x * x - y * y + x0;
                    y = 2.0 * x * y + y0;
                    x = xt;
                    n += 1;
                }
                if n >= 200 {
                    [0, 0, 0]
                } else {
                    let s = n as f32 / 200.0;
                    hsv(0.6 + s * 2.0, 0.8, (s * 3.0).min(1.0))
                }
            })
        })
        .collect();
    Clip {
        name: "mandelbrot",
        frames,
    }
}

/// Pixel-authored UI: a handful of flat colours, hard edges, one moving
/// element. Must come out bit-exact.
fn textui() -> Clip {
    const BG: [u8; 3] = [8, 8, 16];
    const FG: [u8; 3] = [230, 230, 235];
    const ACCENT: [u8; 3] = [255, 140, 0];
    const DIM: [u8; 3] = [70, 70, 90];
    let frames = (0..NFRAMES)
        .map(|i| {
            let mut f = Frame::black();
            for y in 0..H {
                for x in 0..W {
                    f.set(x, y, BG);
                }
            }
            // Title bar.
            for x in 0..W {
                f.set(x, 0, DIM);
                f.set(x, 1, DIM);
            }
            // Blocky "glyphs": 3x5 cells on a 4x6 grid, two rows.
            let mut seed = 0x1234_5678u32;
            let mut rnd = move || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed
            };
            for row in 0..3 {
                for col in 0..15 {
                    let bits = rnd();
                    for dy in 0..5 {
                        for dx in 0..3 {
                            if (bits >> (dy * 3 + dx)) & 1 == 1 {
                                f.set(2 + col * 4 + dx, 5 + row * 7 + dy, FG);
                            }
                        }
                    }
                }
            }
            // A progress bar that actually moves, so the clip is not static.
            let w = 4 + (i * 56 / NFRAMES);
            for x in 0..60 {
                for y in 27..30 {
                    f.set(2 + x, y, if x < w { ACCENT } else { DIM });
                }
            }
            f
        })
        .collect();
    Clip {
        name: "textui",
        frames,
    }
}

/// Photographic: a soft sky gradient, a few out-of-focus blobs and film-like
/// grain. Many colours, low structure - the block codec's home ground.
fn photo() -> Clip {
    let frames = (0..NFRAMES)
        .map(|i| {
            let t = i as f32 * 0.05;
            supersample(|x, y| {
                let sky = 1.0 - y / H as f32;
                let mut c = [
                    0.22 + 0.5 * sky,
                    0.33 + 0.45 * sky,
                    0.55 + 0.4 * sky,
                ];
                for (k, (bx, by, r, tint)) in [
                    (18.0f32, 20.0f32, 11.0f32, [0.9f32, 0.55, 0.2]),
                    (44.0, 14.0, 8.0, [0.35, 0.7, 0.3]),
                    (32.0, 26.0, 14.0, [0.2, 0.2, 0.28]),
                ]
                .into_iter()
                .enumerate()
                {
                    let dx = x - bx - (t * 3.0 * (k as f32 + 1.0)).sin() * 2.0;
                    let dy = y - by;
                    let d = (dx * dx + dy * dy).sqrt() / r;
                    let w = (1.0 - d).clamp(0.0, 1.0).powf(1.7);
                    for ch in 0..3 {
                        c[ch] = c[ch] * (1.0 - w) + tint[ch] * w;
                    }
                }
                // Deterministic grain.
                let g = ((x * 12.9898 + y * 78.233 + t * 43.1).sin() * 43758.547).fract();
                for ch in &mut c {
                    *ch = (*ch + (g - 0.5) * 0.045).clamp(0.0, 1.0);
                }
                [
                    (c[0] * 255.0 + 0.5) as u8,
                    (c[1] * 255.0 + 0.5) as u8,
                    (c[2] * 255.0 + 0.5) as u8,
                ]
            })
        })
        .collect();
    Clip {
        name: "photo",
        frames,
    }
}

/// A dark scene fading down: where the panel's bit depth is coarsest and
/// where codec precision is most often wasted.
fn darkfade() -> Clip {
    let frames = (0..NFRAMES)
        .map(|i| {
            let k = 1.0 - i as f32 / NFRAMES as f32 * 0.9;
            supersample(|x, y| {
                let d = ((x - 32.0).powi(2) / 400.0 + (y - 16.0).powi(2) / 120.0).sqrt();
                let v = ((1.2 - d).clamp(0.0, 1.0) * k * 0.28).clamp(0.0, 1.0);
                [
                    (v * 255.0 * 1.0 + 0.5) as u8,
                    (v * 255.0 * 0.7 + 0.5) as u8,
                    (v * 255.0 * 1.3).min(255.0) as u8,
                ]
            })
        })
        .collect();
    Clip {
        name: "darkfade",
        frames,
    }
}

/// The whole corpus.
pub fn all() -> Vec<Clip> {
    vec![plasma(), mandelbrot(), textui(), photo(), darkfade()]
}
