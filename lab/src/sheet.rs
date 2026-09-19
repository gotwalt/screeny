//! Contact sheets: nearest-neighbour upscaled tiles with labels, so a human
//! can check that the numbers agree with their eyes.

use crate::font;
use crate::frame::{Frame, H, W};
use std::path::Path;

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Canvas {
    pub fn new(w: usize, h: usize, bg: [u8; 3]) -> Self {
        let mut px = vec![0u8; w * h * 3];
        for i in 0..w * h {
            px[i * 3] = bg[0];
            px[i * 3 + 1] = bg[1];
            px[i * 3 + 2] = bg[2];
        }
        Canvas { w, h, px }
    }

    pub fn set(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let o = (y as usize * self.w + x as usize) * 3;
        self.px[o] = c[0];
        self.px[o + 1] = c[1];
        self.px[o + 2] = c[2];
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: [u8; 3]) {
        for j in 0..h {
            for i in 0..w {
                self.set(x + i, y + j, c);
            }
        }
    }

    /// Blit a 64x32 frame, nearest-neighbour upscaled `s` times.
    pub fn blit(&mut self, f: &Frame, ox: i32, oy: i32, s: i32) {
        for y in 0..H {
            for x in 0..W {
                let c = f.get(x, y);
                for j in 0..s {
                    for i in 0..s {
                        self.set(ox + x as i32 * s + i, oy + y as i32 * s + j, c);
                    }
                }
            }
        }
    }

    pub fn text(&mut self, s: &str, ox: i32, oy: i32, scale: i32, c: [u8; 3]) {
        font::draw_text(s, 0, 0, |x, y| {
            for j in 0..scale {
                for i in 0..scale {
                    self.set(ox + x * scale + i, oy + y * scale + j, c);
                }
            }
        });
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let file = std::fs::File::create(path)?;
        let w = std::io::BufWriter::new(file);
        let mut enc = png::Encoder::new(w, self.w as u32, self.h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        writer
            .write_image_data(&self.px)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    }
}

pub struct Tile<'a> {
    pub frame: &'a Frame,
    pub label: String,
}

/// Lay out tiles in a grid with `cols` columns at upscale `s`.
pub fn grid(title: &str, tiles: &[Tile], cols: usize, s: i32) -> Canvas {
    const PAD: i32 = 8;
    const LBL: i32 = 17;
    let tw = W as i32 * s;
    let th = H as i32 * s;
    let rows = tiles.len().div_ceil(cols) as i32;
    let cw = tw + PAD;
    let chh = th + LBL + PAD;
    let width = (cols as i32 * cw + PAD) as usize;
    let height = (rows * chh + PAD + 16) as usize;
    let mut c = Canvas::new(width, height, [24, 24, 28]);
    c.text(title, PAD, PAD - 2, 2, [255, 255, 255]);
    for (i, t) in tiles.iter().enumerate() {
        let col = (i % cols) as i32;
        let row = (i / cols) as i32;
        let x = PAD + col * cw;
        let y = PAD + 16 + row * chh;
        c.text(&t.label, x, y, 2, [210, 215, 225]);
        c.rect(x - 1, y + LBL - 1, tw + 2, th + 2, [70, 70, 80]);
        c.blit(t.frame, x, y + LBL, s);
    }
    c
}
