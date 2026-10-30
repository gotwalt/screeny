//! The veil as a membrane: a grid of heights towards us, pinned flat at the
//! edge of the frame, that cannot pass through whatever presses from behind.
//! It has mass, tension and damping and is stepped as a wave, so it drapes
//! over the real shape of the hand - webbing between spread fingers, tenting
//! off a fingertip - and when the hand lets go it springs back and shivers
//! on its own.

/// Cells per side, over `-EXTENT..EXTENT` in x and y. The Art Frame's 32
/// LEDs span 2.16 units, so this is about four cells per LED.
pub const N: usize = 144;
pub const EXTENT: f32 = 1.2;

/// Cell size, in sheet units.
const DX: f32 = 2.0 * EXTENT / (N - 1) as f32;
/// How fast a ripple runs across the sheet (units a second), and how quickly
/// it dies (per second).
const WAVE: f32 = 2.4;
const DAMP: f32 = 4.0;
/// Damping that grows with how fine a ripple is, as cloth's own does: a
/// broad wave keeps its shiver, a cell-sized one dies at once. In units a
/// cell, kept inside what an explicit step allows.
const FINE: f32 = 0.08;
/// The fastest the hand can carry the cloth towards us, units a second.
const PUSH: f32 = 2.0;
/// The longest step. The wave alone is stable up to 0.7 DX / WAVE, but with
/// `FINE` damping the finest ripples grow unless the step is about half that.
const STEP: f32 = 0.3 * DX / WAVE;

pub struct Cloth {
    /// The sheet, and how fast each cell of it is moving.
    pub h: Vec<f32>,
    v: Vec<f32>,
    dv: Vec<f32>,
    /// The front of whatever is behind it, or `NOTHING`.
    pub under: Vec<f32>,
    /// The hand's own light as it comes through the sheet (see `shine`).
    pub glow: Vec<f32>,
    layer: Vec<f32>,
    scratch: Vec<f32>,
}

/// `under` where nothing is behind the sheet.
const NOTHING: f32 = -9.0;

/// How far behind the cloth (sheet units) the hand's light has faded by
/// 1/e, and the depths at which it has spread to each blur radius (cells).
/// The panel's diffuser is white with the LEDs off, so light is all there
/// is to see the hand by: pressed to the cloth it is a sharp bright print,
/// further back a soft and fainter bloom, as through frosted glass.
const FADE: f32 = 0.12;
const BANDS: [(f32, usize); 5] = [(0.0, 0), (0.03, 2), (0.09, 5), (0.2, 10), (0.4, 18)];

pub fn cell(v: f32) -> f32 {
    (v + EXTENT) / (2.0 * EXTENT) * (N - 1) as f32
}

impl Cloth {
    pub fn new() -> Self {
        Cloth {
            h: vec![0.0; N * N],
            v: vec![0.0; N * N],
            dv: vec![0.0; N * N],
            under: vec![NOTHING; N * N],
            glow: vec![0.0; N * N],
            layer: vec![0.0; N * N],
            scratch: vec![0.0; N * N],
        }
    }

    pub fn clear_under(&mut self) {
        self.under.fill(NOTHING);
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

    /// Moves the sheet on by `dt` seconds over what is under it now. `stiff`
    /// pulls the cloth back towards flat, so a tent dies away over a short
    /// distance instead of lifting the whole sheet. Where the hand is, the
    /// cloth goes with it and takes its speed.
    pub fn step(&mut self, dt: f32, stiff: f32) {
        let steps = (dt / STEP).ceil().clamp(1.0, 80.0) as usize;
        let h = dt / steps as f32;
        let c2 = WAVE * WAVE / (DX * DX);
        for _ in 0..steps {
            for y in 1..N - 1 {
                for x in 1..N - 1 {
                    let i = y * N + x;
                    let lap = self.h[i - 1] + self.h[i + 1] + self.h[i - N] + self.h[i + N] - 4.0 * self.h[i];
                    let lap_v = self.v[i - 1] + self.v[i + 1] + self.v[i - N] + self.v[i + N] - 4.0 * self.v[i];
                    let accel = c2 * (lap - stiff * self.h[i]) - DAMP * self.v[i];
                    self.dv[i] = accel * h + FINE * lap_v;
                }
            }
            for i in 0..N * N {
                self.v[i] += self.dv[i];
                let was = self.h[i];
                self.h[i] += self.v[i] * h;
                if self.h[i] < self.under[i] {
                    self.h[i] = self.under[i];
                    // No faster than a hand moves, so a surface that appears
                    // all at once does not fling the cloth.
                    self.v[i] = self.v[i].max(((self.under[i] - was) / h).min(PUSH));
                }
            }
        }
    }

    /// Works out `glow`: each cell of the hand gives light that fades and
    /// spreads with how far behind the cloth it is.
    pub fn shine(&mut self) {
        self.glow.fill(0.0);
        for band in 0..BANDS.len() {
            // Each cell's light goes to the two bands its depth lies between.
            let mut any = false;
            for i in 0..N * N {
                self.layer[i] = 0.0;
                if self.under[i] <= NOTHING {
                    continue;
                }
                let gap = (self.h[i] - self.under[i]).max(0.0);
                let near = BANDS[band].0;
                let w = if band + 1 < BANDS.len() && gap >= near {
                    1.0 - ((gap - near) / (BANDS[band + 1].0 - near)).min(1.0)
                } else if band > 0 && gap < near {
                    let prev = BANDS[band - 1].0;
                    ((gap - prev) / (near - prev)).max(0.0)
                } else if band + 1 == BANDS.len() && gap >= near {
                    1.0
                } else {
                    0.0
                };
                if w > 0.0 {
                    self.layer[i] = w * (-gap / FADE).exp();
                    any = true;
                }
            }
            if !any {
                continue;
            }
            let r = BANDS[band].1;
            if r > 0 {
                // Twice a box is close enough to a gaussian.
                for _ in 0..2 {
                    blur(&mut self.layer, &mut self.scratch, r, 1, N);
                    blur(&mut self.layer, &mut self.scratch, r, N, 1);
                }
            }
            for i in 0..N * N {
                self.glow[i] += self.layer[i];
            }
        }
    }

    /// Height and how firmly the cloth lies on the hand, at a point in sheet
    /// units, bilinear between cells.
    pub fn at(&self, x: f32, y: f32) -> (f32, f32, f32) {
        let (fx, fy) = (cell(x).clamp(0.0, (N - 2) as f32), cell(y).clamp(0.0, (N - 2) as f32));
        let (ix, iy) = (fx as usize, fy as usize);
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let mut h = 0.0;
        let mut touch = 0.0;
        let mut glow = 0.0;
        for (dx, dy, w) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
            let i = (iy + dy) * N + ix + dx;
            h += self.h[i] * w;
            glow += self.glow[i] * w;
            let gap = self.h[i] - self.under[i];
            touch += if self.under[i] > 0.0 { (1.0 - gap / 0.02).clamp(0.0, 1.0) * w } else { 0.0 };
        }
        (h, touch, glow)
    }
}

/// A box blur of radius `r` cells along one axis of an N x N grid: `step`
/// apart along the axis, `across` between lines.
fn blur(a: &mut [f32], tmp: &mut [f32], r: usize, step: usize, across: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..N {
        let at = |k: usize| line * across + k * step;
        let mut sum = 0.0;
        for k in 0..=r.min(N - 1) {
            sum += a[at(k)];
        }
        sum += a[at(0)] * r as f32;
        for k in 0..N {
            tmp[at(k)] = sum * norm;
            let add = a[at((k + r + 1).min(N - 1))];
            let drop = a[at(k.saturating_sub(r))];
            sum += add - drop;
        }
    }
    a.copy_from_slice(tmp);
}
