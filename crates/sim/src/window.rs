//! The LED-dot view.
//!
//! `docs/design/generative-art-brief.md` section 5: round dots on black with
//! gaps, dot diameter about 60-70% of the pitch, upscaled at least 12x, and a
//! slight bloom on bright ones. A nearest-neighbour upscale "lies to you: it
//! makes dither look coarser and thin lines look more solid than the real
//! thing", which is the whole reason this file exists rather than a
//! `set_scale(X16)` call.
//!
//! The panel model has already been applied by the time a frame gets here -
//! [`crate::panel`] does it on the frame thread - so this module only draws.

#![cfg(feature = "window")]

use screeny_proto::{Rgb888Frame, H, W};

use crate::font;

/// Default upscale. 14 puts a 64x32 panel in 896x448, a little over the
/// 19x10 cm the real one measures on a typical display.
pub const DEFAULT_SCALE: usize = 14;
/// Height of the statistics strip under the panel, pixels.
pub const OVERLAY_H: usize = 78;

/// A precomputed dot stamp and the ARGB buffer minifb wants.
pub struct Led {
    scale: usize,
    /// One cell's worth of 0..=4096 weights: the antialiased dot plus its
    /// halo.
    stamp: Vec<u16>,
    /// `scale * W` by `scale * H + OVERLAY_H`, 0RGB.
    pub buf: Vec<u32>,
}

impl Led {
    /// Build the stamp for one upscale factor.
    #[must_use]
    pub fn new(scale: usize) -> Self {
        let scale = scale.max(4);
        let mut stamp = vec![0u16; scale * scale];
        let c = scale as f32 / 2.0;
        // 65% of the pitch, so a third of the cell is the gap between dots.
        let dot_r = scale as f32 * 0.325;
        // The halo reaches the cell edge and no further, so dots never
        // overlap and the draw stays one write per output pixel.
        let halo_r = scale as f32 * 0.5;
        for y in 0..scale {
            for x in 0..scale {
                let dx = x as f32 + 0.5 - c;
                let dy = y as f32 + 0.5 - c;
                let d = (dx * dx + dy * dy).sqrt();
                // Antialias the rim over one output pixel.
                let core = ((dot_r + 0.5 - d) / 1.0).clamp(0.0, 1.0);
                let halo = if d <= dot_r {
                    0.0
                } else {
                    let t = ((halo_r - d) / (halo_r - dot_r)).clamp(0.0, 1.0);
                    t * t * 0.22
                };
                stamp[y * scale + x] = ((core + halo).min(1.0) * 4096.0) as u16;
            }
        }
        Led {
            scale,
            stamp,
            buf: vec![0u32; scale * W * (scale * H + OVERLAY_H)],
        }
    }

    /// Output width in pixels.
    #[must_use]
    pub fn width(&self) -> usize {
        self.scale * W
    }

    /// Output height in pixels, panel plus overlay.
    #[must_use]
    pub fn height(&self) -> usize {
        self.scale * H + OVERLAY_H
    }

    /// Draw one panel frame as dots.
    pub fn draw(&mut self, frame: &Rgb888Frame) {
        let s = self.scale;
        let stride = self.width();
        for py in 0..H {
            for px in 0..W {
                let i = (py * W + px) * 3;
                let (r, g, b) = (frame[i] as u32, frame[i + 1] as u32, frame[i + 2] as u32);
                let x0 = px * s;
                let y0 = py * s;
                for dy in 0..s {
                    let row = (y0 + dy) * stride + x0;
                    for dx in 0..s {
                        let w = self.stamp[dy * s + dx] as u32;
                        let rr = (r * w) >> 12;
                        let gg = (g * w) >> 12;
                        let bb = (b * w) >> 12;
                        self.buf[row + dx] = (rr << 16) | (gg << 8) | bb;
                    }
                }
            }
        }
    }

    /// Clear the statistics strip.
    pub fn clear_overlay(&mut self) {
        let start = self.scale * H * self.width();
        for p in &mut self.buf[start..] {
            *p = 0x0C_0C_10;
        }
    }

    /// Draw one line of text in the strip, `line` counting from 0, at double
    /// the font's natural size.
    pub fn overlay_text(&mut self, line: usize, x: usize, s: &str, colour: u32) {
        let scale = 2usize;
        let top = self.scale * H + 4 + line * (font::H5 * scale + 4);
        let stride = self.width();
        let mut at = x;
        for ch in s.chars() {
            let g = font::glyph_5x7(ch);
            for (col, bits) in g.iter().enumerate() {
                for row in 0..font::H5 {
                    if bits >> row & 1 == 0 {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = at + col * scale + dx;
                            let py = top + row * scale + dy;
                            if px < stride && py < self.height() {
                                self.buf[py * stride + px] = colour;
                            }
                        }
                    }
                }
            }
            at += (font::W5 + 1) * scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use screeny_proto::NBYTES;

    #[test]
    fn a_dot_is_round_and_the_gaps_are_dark() {
        let mut led = Led::new(16);
        let frame = [255u8; NBYTES];
        led.draw(&frame);
        let stride = led.width();
        let at = |x: usize, y: usize| led.buf[y * stride + x] & 0xFF;

        // Scale 16: the cell centre is (8, 8) and the dot radius is 5.2 px.
        assert_eq!(at(8, 8), 255, "the centre of a dot");
        assert_eq!(at(11, 8), 255, "still well inside it");
        let rim = at(13, 8);
        assert!((1..255).contains(&rim), "the antialiased rim, got {rim}");
        assert!(at(0, 0) < 30, "the corner between dots, got {}", at(0, 0));

        // And brightness falls away from the centre, never rises.
        let mut prev = 255;
        for x in 8..16 {
            let v = at(x, 8);
            assert!(v <= prev, "brightness rose again at x={x}: {v} > {prev}");
            prev = v;
        }
    }

    #[test]
    fn a_black_panel_is_black() {
        let mut led = Led::new(12);
        led.draw(&[0u8; NBYTES]);
        assert!(led.buf[..led.width() * H * 12].iter().all(|&p| p == 0));
    }

    #[test]
    fn the_buffer_is_the_size_minifb_will_be_told() {
        for scale in [4usize, 12, 14, 20] {
            let led = Led::new(scale);
            assert_eq!(led.buf.len(), led.width() * led.height());
            assert_eq!(led.width(), scale * W);
            assert_eq!(led.height(), scale * H + OVERLAY_H);
        }
        // Below the minimum, the scale is clamped rather than dividing by zero.
        let led = Led::new(0);
        assert_eq!(led.buf.len(), led.width() * led.height());
    }

    #[test]
    fn overlay_text_stays_inside_the_strip() {
        let mut led = Led::new(12);
        led.draw(&[0u8; NBYTES]);
        led.clear_overlay();
        led.overlay_text(0, 6, "30.0 fps  PAL8_LZ  892 B", 0xE0E0E0);
        led.overlay_text(3, 6, "a line below the strip", 0xE0E0E0);
        led.overlay_text(0, 10_000, "off the right edge", 0xE0E0E0);

        // The panel above is untouched.
        let panel = led.width() * H * 12;
        assert!(led.buf[..panel].iter().all(|&p| p == 0));
        // And something was drawn in the strip.
        assert!(led.buf[panel..].contains(&0xE0E0E0));
    }
}
