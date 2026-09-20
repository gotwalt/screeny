//! The preview: what the *panel* will show, not what the framebuffer contains.
//!
//! Pipeline, per the brief's section 5:
//!
//! 1. take the 64x32 sRGB888 frame,
//! 2. apply the panel model (sRGB -> linear -> `levels` steps -> back),
//! 3. draw each pixel as a round dot on black with gaps (dot diameter ~65% of
//!    pitch) at >= 10x, with a little bloom,
//! 4. optionally blur the whole thing ("squint") to approximate the way the
//!    eye merges the LEDs at 1-3 m.
//!
//! A nearest-neighbour upscale lies: it makes dither look coarser and thin
//! lines look more solid than they are. Everything here is composited in
//! linear light.

use crate::color::{lin_to_srgb8, srgb8_to_lin};
use crate::font;
use crate::frame::{Frame, H, W};
use crate::panel::Panel;
use std::path::Path;

#[derive(Clone, Copy)]
pub struct PreviewOpts {
    pub panel: Panel,
    /// Output pixels per LED pitch.
    pub scale: usize,
    /// Dot diameter as a fraction of the pitch.
    pub dot: f32,
    /// Halo strength, as a fraction of the LED's own light.
    pub bloom: f32,
    /// Light level of an *unlit* LED. The Tidbyt's LEDs are visible objects on
    /// a dark mask (see `docs/research/img/stock-word-clock.jpg`), and showing
    /// them keeps the preview honest about how much of the panel is grid.
    pub mask: f32,
}

impl Default for PreviewOpts {
    fn default() -> Self {
        PreviewOpts {
            panel: crate::panel::NOMINAL,
            scale: 12,
            dot: 0.58,
            bloom: 0.20,
            mask: 0.010,
        }
    }
}

/// An sRGB8 image (the preview PNG, not a panel frame).
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Img {
    pub fn new(w: usize, h: usize, bg: [u8; 3]) -> Self {
        let mut px = vec![0u8; w * h * 3];
        for i in 0..w * h {
            px[i * 3] = bg[0];
            px[i * 3 + 1] = bg[1];
            px[i * 3 + 2] = bg[2];
        }
        Img { w, h, px }
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

    pub fn blit(&mut self, src: &Img, ox: i32, oy: i32) {
        for y in 0..src.h {
            for x in 0..src.w {
                let o = (y * src.w + x) * 3;
                self.set(
                    ox + x as i32,
                    oy + y as i32,
                    [src.px[o], src.px[o + 1], src.px[o + 2]],
                );
            }
        }
    }

    pub fn text(&mut self, s: &str, ox: i32, oy: i32, scale: i32, c: [u8; 3]) {
        font::draw(s, 0, 0, |x, y| {
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
        // png 0.18 renamed the levels; High is what Best meant (card 016 put the
        // whole workspace on one png version).
        enc.set_compression(png::Compression::High);
        let mut writer = enc
            .write_header()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        writer
            .write_image_data(&self.px)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    }
}

/// Linear-light accumulation buffer used while compositing dots.
struct LinImg {
    w: usize,
    h: usize,
    px: Vec<[f32; 3]>,
}

impl LinImg {
    fn to_img(&self) -> Img {
        let mut img = Img::new(self.w, self.h, [0, 0, 0]);
        for i in 0..self.w * self.h {
            let p = self.px[i];
            img.px[i * 3] = lin_to_srgb8(p[0]);
            img.px[i * 3 + 1] = lin_to_srgb8(p[1]);
            img.px[i * 3 + 2] = lin_to_srgb8(p[2]);
        }
        img
    }

    /// Separable box blur, repeated 3x ~= Gaussian. `r` in output pixels.
    fn blur(&mut self, r: usize) {
        if r == 0 {
            return;
        }
        for _ in 0..3 {
            self.blur1(r, true);
            self.blur1(r, false);
        }
    }

    fn blur1(&mut self, r: usize, horiz: bool) {
        let (n, m) = if horiz {
            (self.w, self.h)
        } else {
            (self.h, self.w)
        };
        let mut line = vec![[0f32; 3]; n];
        for j in 0..m {
            for (i, slot) in line.iter_mut().enumerate() {
                *slot = if horiz {
                    self.px[j * self.w + i]
                } else {
                    self.px[i * self.w + j]
                };
            }
            for i in 0..n {
                let lo = i.saturating_sub(r);
                let hi = (i + r).min(n - 1);
                let mut acc = [0f32; 3];
                // In order, one sample at a time: the sum is a float and
                // reassociating it would move pixels.
                for s in &line[lo..=hi] {
                    acc[0] += s[0];
                    acc[1] += s[1];
                    acc[2] += s[2];
                }
                let c = (hi - lo + 1) as f32;
                let v = [acc[0] / c, acc[1] / c, acc[2] / c];
                if horiz {
                    self.px[j * self.w + i] = v;
                } else {
                    self.px[i * self.w + j] = v;
                }
            }
        }
    }
}

fn composite(frame: &Frame, o: &PreviewOpts) -> LinImg {
    let s = o.scale;
    let mut img = LinImg {
        w: W * s,
        h: H * s,
        px: vec![[0.0; 3]; W * s * H * s],
    };
    let r = o.dot * s as f32 / 2.0;
    let sigma = 0.40 * s as f32;
    let reach = (r + 2.0 * sigma.max(1.0)).ceil() as i32;
    for y in 0..H {
        for x in 0..W {
            let emitted = o.panel.emit(frame.get(x, y));
            let core = [
                emitted[0] + o.mask,
                emitted[1] + o.mask,
                emitted[2] + o.mask,
            ];
            let cx = x as f32 * s as f32 + s as f32 / 2.0 - 0.5;
            let cy = y as f32 * s as f32 + s as f32 / 2.0 - 0.5;
            let x0 = (cx as i32 - reach).max(0);
            let x1 = (cx as i32 + reach).min(img.w as i32 - 1);
            let y0 = (cy as i32 - reach).max(0);
            let y1 = (cy as i32 + reach).min(img.h as i32 - 1);
            for py in y0..=y1 {
                for px in x0..=x1 {
                    let dx = px as f32 - cx;
                    let dy = py as f32 - cy;
                    let d = (dx * dx + dy * dy).sqrt();
                    // Anti-aliased disc, plus a soft halo scaled by the LED's
                    // own output (bright dots bloom, dim ones barely do).
                    let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                    let halo = o.bloom * (-(d * d) / (2.0 * sigma * sigma)).exp();
                    // Truncate the invisible tail of the halo: it is below
                    // one sRGB code everywhere out there, and carrying it
                    // triples the size of a saved PNG.
                    if cov + halo <= 0.004 {
                        continue;
                    }
                    let p = &mut img.px[py as usize * img.w + px as usize];
                    for k in 0..3 {
                        p[k] += core[k] * cov + emitted[k] * halo;
                    }
                }
            }
        }
    }
    img
}

/// The panel view: round dots on black at `scale` x.
pub fn render(frame: &Frame, o: &PreviewOpts) -> Img {
    composite(frame, o).to_img()
}

/// The squint view: the same thing blurred, at half the scale, as if seen from
/// across the room.
pub fn render_squint(frame: &Frame, o: &PreviewOpts) -> Img {
    let mut lin = composite(frame, o);
    lin.blur(o.scale / 3 + 1);
    let full = lin.to_img();
    // Halve it so the squint tile sits beside the sharp one without dwarfing it.
    let (w, h) = (full.w / 2, full.h / 2);
    let mut out = Img::new(w, h, [0, 0, 0]);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0f32; 3];
            for j in 0..2 {
                for i in 0..2 {
                    let o2 = ((y * 2 + j) * full.w + x * 2 + i) * 3;
                    let l = srgb8_to_lin([full.px[o2], full.px[o2 + 1], full.px[o2 + 2]]);
                    acc[0] += l[0];
                    acc[1] += l[1];
                    acc[2] += l[2];
                }
            }
            out.set(
                x as i32,
                y as i32,
                [
                    lin_to_srgb8(acc[0] / 4.0),
                    lin_to_srgb8(acc[1] / 4.0),
                    lin_to_srgb8(acc[2] / 4.0),
                ],
            );
        }
    }
    out
}

pub struct Tile {
    pub label: String,
    pub img: Img,
}

/// Lay out labelled tiles in a grid.
pub fn sheet(title: &str, tiles: Vec<Tile>, cols: usize) -> Img {
    const PAD: i32 = 10;
    const LBL: i32 = 16;
    let tw = tiles.iter().map(|t| t.img.w).max().unwrap_or(1) as i32;
    let th = tiles.iter().map(|t| t.img.h).max().unwrap_or(1) as i32;
    let rows = tiles.len().div_ceil(cols) as i32;
    let cw = tw + PAD;
    let ch = th + LBL + PAD;
    let width = (cols as i32 * cw + PAD) as usize;
    let height = (rows * ch + PAD + 22) as usize;
    let mut c = Img::new(width, height, [16, 16, 19]);
    c.text(title, PAD, PAD - 2, 2, [235, 235, 245]);
    for (i, t) in tiles.iter().enumerate() {
        let col = (i % cols) as i32;
        let row = (i / cols) as i32;
        let x = PAD + col * cw;
        let y = PAD + 22 + row * ch;
        c.text(&t.label, x, y, 1, [170, 178, 195]);
        c.rect(
            x - 1,
            y + LBL - 1,
            t.img.w as i32 + 2,
            t.img.h as i32 + 2,
            [60, 60, 70],
        );
        c.blit(&t.img, x, y + LBL);
    }
    c
}

/// Write an animated PNG (one file, plays in a browser; the Read tool shows
/// the first frame, which is why the sheet exists too).
pub fn save_apng(path: &Path, frames: &[Img], fps: u16) -> std::io::Result<()> {
    assert!(!frames.is_empty());
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(w, frames[0].w as u32, frames[0].h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_animated(frames.len() as u32, 0)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    enc.set_frame_delay(1, fps)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut writer = enc
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    for f in frames {
        writer
            .write_image_data(&f.px)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }
    Ok(())
}
