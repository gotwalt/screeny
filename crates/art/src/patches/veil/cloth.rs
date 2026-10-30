//! The veil as a membrane: a grid of heights towards us, pinned flat at the
//! edge of the frame, that cannot pass through whatever presses from behind.
//! Solved by relaxation each frame from the last frame's sheet, so it drapes
//! over the real shape of the hand - webbing between spread fingers, tenting
//! off a fingertip - and lags a little when the hand moves fast.

/// Cells per side, over `-EXTENT..EXTENT` in x and y. The Art Frame's 32
/// LEDs span 2.16 units, so this is about four cells per LED.
pub const N: usize = 144;
pub const EXTENT: f32 = 1.2;

/// Relaxation sweeps per frame, and the over-relaxation factor.
const SWEEPS: usize = 36;
const OMEGA: f32 = 1.85;

pub struct Cloth {
    /// The sheet.
    pub h: Vec<f32>,
    /// The front of whatever is behind it, or -1 where nothing is.
    pub under: Vec<f32>,
}

pub fn cell(v: f32) -> f32 {
    (v + EXTENT) / (2.0 * EXTENT) * (N - 1) as f32
}

impl Cloth {
    pub fn new() -> Self {
        Cloth { h: vec![0.0; N * N], under: vec![-1.0; N * N] }
    }

    pub fn clear_under(&mut self) {
        self.under.fill(-1.0);
    }

    /// Lays one triangle of the hand (x, y in sheet units, z towards us) into
    /// `under`, keeping the front-most surface.
    pub fn press(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3]) {
        let (ax, ay, bx, by, cx, cy) = (cell(a[0]), cell(a[1]), cell(b[0]), cell(b[1]), cell(c[0]), cell(c[1]));
        let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
        if area.abs() < 1e-6 {
            return;
        }
        let x0 = ax.min(bx).min(cx).ceil().max(1.0) as usize;
        let x1 = ax.max(bx).max(cx).floor().min((N - 2) as f32);
        let y0 = ay.min(by).min(cy).ceil().max(1.0) as usize;
        let y1 = ay.max(by).max(cy).floor().min((N - 2) as f32);
        if x1 < x0 as f32 || y1 < y0 as f32 {
            return;
        }
        for y in y0..=y1 as usize {
            for x in x0..=x1 as usize {
                let (px, py) = (x as f32, y as f32);
                let wa = ((bx - px) * (cy - py) - (by - py) * (cx - px)) / area;
                let wb = ((cx - px) * (ay - py) - (cy - py) * (ax - px)) / area;
                let wc = 1.0 - wa - wb;
                if wa < -1e-4 || wb < -1e-4 || wc < -1e-4 {
                    continue;
                }
                let z = wa * a[2] + wb * b[2] + wc * c[2];
                let u = &mut self.under[y * N + x];
                *u = u.max(z);
            }
        }
    }

    /// Relax the sheet over what is under it. `stiff` pulls the cloth back
    /// towards flat, so a tent dies away over a short distance instead of
    /// lifting the whole sheet.
    pub fn settle(&mut self, stiff: f32) {
        let denom = 4.0 + stiff;
        for _ in 0..SWEEPS {
            for y in 1..N - 1 {
                for x in 1..N - 1 {
                    let i = y * N + x;
                    let around = self.h[i - 1] + self.h[i + 1] + self.h[i - N] + self.h[i + N];
                    let relaxed = self.h[i] + OMEGA * (around / denom - self.h[i]);
                    self.h[i] = relaxed.max(self.under[i]).max(0.0);
                }
            }
        }
    }

    /// Height and how firmly the cloth lies on the hand, at a point in sheet
    /// units, bilinear between cells.
    pub fn at(&self, x: f32, y: f32) -> (f32, f32) {
        let (fx, fy) = (cell(x).clamp(0.0, (N - 2) as f32), cell(y).clamp(0.0, (N - 2) as f32));
        let (ix, iy) = (fx as usize, fy as usize);
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let mut h = 0.0;
        let mut touch = 0.0;
        for (dx, dy, w) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
            let i = (iy + dy) * N + ix + dx;
            h += self.h[i] * w;
            let gap = self.h[i] - self.under[i];
            touch += if self.under[i] > 0.0 { (1.0 - gap / 0.02).clamp(0.0, 1.0) * w } else { 0.0 };
        }
        (h, touch)
    }
}
