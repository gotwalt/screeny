//! The fire itself: a 2D Eulerian gas on a collocated grid, stepped by the
//! textbook "stable fluids" splitting.
//!
//! Each step, in order:
//!
//! 1. **Advect** every field - velocity, temperature, fuel, smoke - by one
//!    semi-Lagrangian backtrace (Stam 1999), a midpoint trace so curling flow
//!    is followed rather than cut across. One trace per cell serves all five
//!    fields.
//! 2. **Combust** (after Nguyen, Fedkiw and Jensen 2002, in its simplest
//!    form): fuel burns away at a fixed rate, the gas it is in is held at
//!    least as hot as the fuel can make it, everything else cools by a
//!    radiative `theta^4` term plus a small linear one, and burnt fuel leaves
//!    smoke, which slowly thins.
//! 3. **Forces**: buoyancy from heat, a little weight from smoke (Fedkiw,
//!    Stam and Jensen 2001), the wind the patch hands in, and a gentle drag so
//!    the gas never piles up speed.
//! 4. **Vorticity confinement** (Fedkiw et al. 2001): the grid's numerical
//!    diffusion smooths away the small curls that make a flame tongue lick;
//!    this puts them back, pushing along the gradient of curl magnitude.
//! 5. **Projection**: divergence, a pressure solve, subtract the gradient, so
//!    the gas is incompressible. The solve is red-black Gauss-Seidel with
//!    over-relaxation for a **fixed** number of sweeps, warm-started from last
//!    step's pressure - the same work and the same answer every time, which a
//!    convergence test would not promise.
//!
//! The pipeline is the one `olliep24/firesim` runs on a 128^3 GPU grid (it
//! was the reference the owner pointed at); none of its code is here, and
//! this runs on one CPU core in plain `f32` arrays. Row 0 is the bottom.
//!
//! Boundaries: the top and both sides are open (pressure 0, values carried
//! out by the clamped sampling), so gusts blow through and smoke leaves; the
//! bottom is the hearth floor, a closed wall.

/// What one step needs from the patch: the per-cell fuel the logs are
/// putting in, and the air's horizontal drift.
pub struct Drive<'a> {
    /// The fuel level the logs hold each bed cell at (a floor, `0..1.5`),
    /// row-major from the bottom. The bed is kept saturated where it burns,
    /// so the flames' height is set by how long fuel lasts as it rises, not
    /// by how much comes in - which the sim answers far less steadily. Only the
    /// first `bed_rows` rows are read.
    pub inject: &'a [f32],
    /// How hot the coals hold the gas among them, per bed cell: a floor on
    /// temperature. Glowing embers heat the air without flaming.
    pub coals: &'a [f32],
    pub bed_rows: usize,
    /// The wind's horizontal speed, cells per second, per row.
    pub wind: &'a [f32],
}

/// The physical constants, in cells and seconds. Tuned by eye.
#[derive(Clone, Copy, Debug)]
pub struct Tune {
    /// Upward acceleration per unit temperature.
    pub buoyancy: f32,
    /// Downward pull per unit smoke.
    pub smoke_weight: f32,
    /// Velocity lost per second, as a rate.
    pub drag: f32,
    /// How quickly the gas takes up the wind's speed, per second.
    pub wind_grip: f32,
    /// Vorticity confinement strength.
    pub confine: f32,
    /// Fuel burnt per second, as a rate.
    pub burn: f32,
    /// How hot a unit of fuel holds its gas (temperature is clamped at 1).
    pub fuel_heat: f32,
    /// Radiative (`theta^4`) and linear cooling, per second.
    pub cool4: f32,
    pub cool1: f32,
    /// How much burning gas expands: the divergence the projection is asked
    /// to leave where fuel burns, per unit burnt per second (Nguyen et al.'s
    /// expansion at the flame front). It is what drives the gas up off the
    /// logs.
    pub expand: f32,
    /// Smoke left by a unit of burnt fuel, and how fast smoke thins.
    pub smoke_yield: f32,
    pub smoke_fade: f32,
}

pub const SWEEPS: usize = 22;
const SOR: f32 = 1.72;

pub struct Fluid {
    pub w: usize,
    pub h: usize,
    pub u: Vec<f32>,
    pub v: Vec<f32>,
    pub temp: Vec<f32>,
    pub fuel: Vec<f32>,
    pub smoke: Vec<f32>,
    pressure: Vec<f32>,
    // Scratch, kept so a step allocates nothing.
    nu: Vec<f32>,
    nv: Vec<f32>,
    nt: Vec<f32>,
    nf: Vec<f32>,
    ns: Vec<f32>,
    curl: Vec<f32>,
    div: Vec<f32>,
    source: Vec<f32>,
}

#[inline]
fn sample(f: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let x0 = (x as usize).min(w - 2);
    let y0 = (y as usize).min(h - 2);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let i = y0 * w + x0;
    let a = f[i] + (f[i + 1] - f[i]) * fx;
    let b = f[i + w] + (f[i + w + 1] - f[i + w]) * fx;
    a + (b - a) * fy
}

impl Fluid {
    pub fn new(w: usize, h: usize) -> Fluid {
        let z = || vec![0.0; w * h];
        Fluid {
            w,
            h,
            u: z(),
            v: z(),
            temp: z(),
            fuel: z(),
            smoke: z(),
            pressure: z(),
            nu: z(),
            nv: z(),
            nt: z(),
            nf: z(),
            ns: z(),
            curl: z(),
            div: z(),
            source: z(),
        }
    }

    /// Velocity at a real-valued position, cells per second.
    pub fn velocity(&self, x: f32, y: f32) -> (f32, f32) {
        (sample(&self.u, self.w, self.h, x, y), sample(&self.v, self.w, self.h, x, y))
    }

    pub fn temperature(&self, x: f32, y: f32) -> f32 {
        sample(&self.temp, self.w, self.h, x, y)
    }

    pub fn step(&mut self, dt: f32, tune: &Tune, drive: &Drive) {
        self.advect(dt);
        self.combust(dt, tune, drive);
        self.forces(dt, tune, drive);
        self.confine(dt, tune);
        self.project();
    }

    fn advect(&mut self, dt: f32) {
        let (w, h) = (self.w, self.h);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let (px, py) = (x as f32, y as f32);
                let (mx, my) = (px - 0.5 * dt * self.u[i], py - 0.5 * dt * self.v[i]);
                let (mu, mv) = (sample(&self.u, w, h, mx, my), sample(&self.v, w, h, mx, my));
                let (sx, sy) = (px - dt * mu, py - dt * mv);
                self.nu[i] = sample(&self.u, w, h, sx, sy);
                self.nv[i] = sample(&self.v, w, h, sx, sy);
                self.nt[i] = sample(&self.temp, w, h, sx, sy);
                self.nf[i] = sample(&self.fuel, w, h, sx, sy);
                self.ns[i] = sample(&self.smoke, w, h, sx, sy);
            }
        }
        std::mem::swap(&mut self.u, &mut self.nu);
        std::mem::swap(&mut self.v, &mut self.nv);
        std::mem::swap(&mut self.temp, &mut self.nt);
        std::mem::swap(&mut self.fuel, &mut self.nf);
        std::mem::swap(&mut self.smoke, &mut self.ns);
    }

    fn combust(&mut self, dt: f32, tune: &Tune, drive: &Drive) {
        let w = self.w;
        let bed = drive.bed_rows.min(self.h) * w;
        for i in 0..bed {
            let inj = drive.inject[i];
            self.fuel[i] = self.fuel[i].max(inj.min(1.5));
            self.temp[i] = self.temp[i].max(drive.coals[i]);
        }
        let keep = (-tune.burn * dt).exp();
        let thin = (-tune.smoke_fade * dt).exp();
        for i in 0..self.w * self.h {
            let f = self.fuel[i];
            let burnt = f * (1.0 - keep);
            self.fuel[i] = f - burnt;
            self.source[i] = tune.expand * burnt / dt;
            let t = self.temp[i];
            let cooled = t - dt * (tune.cool4 * t * t * t * t + tune.cool1 * t);
            self.temp[i] = cooled.max((f * tune.fuel_heat).min(1.0)).clamp(0.0, 1.0);
            self.smoke[i] = (self.smoke[i] * thin + burnt * tune.smoke_yield).min(2.0);
        }
    }

    fn forces(&mut self, dt: f32, tune: &Tune, drive: &Drive) {
        let w = self.w;
        let keep = (-tune.drag * dt).exp();
        let grip = 1.0 - (-tune.wind_grip * dt).exp();
        for y in 0..self.h {
            let wind = drive.wind[y];
            for x in 0..w {
                let i = y * w + x;
                let lift = tune.buoyancy * self.temp[i] - tune.smoke_weight * self.smoke[i];
                self.v[i] = (self.v[i] + dt * lift) * keep;
                self.u[i] = (self.u[i] + (wind - self.u[i]) * grip) * keep;
            }
        }
        // The hearth floor: nothing flows through it.
        for x in 0..w {
            self.v[x] = self.v[x].max(0.0);
        }
    }

    fn confine(&mut self, dt: f32, tune: &Tune) {
        let (w, h) = (self.w, self.h);
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let i = y * w + x;
                self.curl[i] = 0.5 * ((self.v[i + 1] - self.v[i - 1]) - (self.u[i + w] - self.u[i - w]));
            }
        }
        for y in 2..h - 2 {
            for x in 2..w - 2 {
                let i = y * w + x;
                let gx = 0.5 * (self.curl[i + 1].abs() - self.curl[i - 1].abs());
                let gy = 0.5 * (self.curl[i + w].abs() - self.curl[i - w].abs());
                let len = (gx * gx + gy * gy).sqrt() + 1e-5;
                let (nx, ny) = (gx / len, gy / len);
                let c = self.curl[i];
                self.u[i] += dt * tune.confine * ny * c;
                self.v[i] -= dt * tune.confine * nx * c;
            }
        }
    }

    fn project(&mut self) {
        let (w, h) = (self.w, self.h);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let ur = if x + 1 < w { self.u[i + 1] } else { self.u[i] };
                let ul = if x > 0 { self.u[i - 1] } else { self.u[i] };
                let vu = if y + 1 < h { self.v[i + w] } else { self.v[i] };
                // The floor is a wall: the gas below it does not move.
                let vd = if y > 0 { self.v[i - w] } else { 0.0 };
                self.div[i] = 0.5 * (ur - ul + vu - vd) - self.source[i];
            }
        }
        // Pressure is 0 beyond the open top and sides, and mirrors across the
        // floor.
        let p = &mut self.pressure;
        for _ in 0..SWEEPS {
            for colour in 0..2 {
                for y in 0..h {
                    let start = (y + colour) % 2;
                    for x in (start..w).step_by(2) {
                        let i = y * w + x;
                        let l = if x > 0 { p[i - 1] } else { 0.0 };
                        let r = if x + 1 < w { p[i + 1] } else { 0.0 };
                        let d = if y > 0 { p[i - w] } else { p[i] };
                        let u = if y + 1 < h { p[i + w] } else { 0.0 };
                        let gs = 0.25 * (l + r + d + u - self.div[i]);
                        p[i] += SOR * (gs - p[i]);
                    }
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let l = if x > 0 { p[i - 1] } else { 0.0 };
                let r = if x + 1 < w { p[i + 1] } else { 0.0 };
                let d = if y > 0 { p[i - w] } else { p[i] };
                let u = if y + 1 < h { p[i + w] } else { 0.0 };
                self.u[i] -= 0.5 * (r - l);
                self.v[i] -= 0.5 * (u - d);
            }
        }
    }
}
