//! The dove as a mesh, made here rather than read from a file: a lofted body
//! and head with a short beak, a fanned tail, and two wings whose primaries
//! are separate feathers, so a spread wing ends in fingers. It is posed only
//! by its wings (`Wings`) and built afresh for each pose; at a couple of
//! thousand triangles that costs nothing beside the glass.
//!
//! The bird's own axes: x to its right, y forward (where its head is), z up
//! (its back). A body length, beak to tail, is about 1; the wingspan is
//! `SPAN`.

pub type V3 = [f32; 3];

/// Wingtip to wingtip with the wings spread flat.
pub const SPAN: f32 = 1.75;

/// How the wings are at an instant.
#[derive(Clone, Copy, Debug)]
pub struct Wings {
    /// How far each wing is raised at the shoulder, towards the bird's back
    /// (radians; 0 is flat out to the side).
    pub lift: f32,
    /// How much further the tip is raised than the shoulder: the wing bends
    /// as it beats, the tip trailing the stroke.
    pub bend: f32,
    /// How far the wings are drawn in (0 spread, 1 half folded): on the
    /// upstroke a bird folds them a little.
    pub fold: f32,
}

#[derive(Default)]
pub struct Mesh {
    pub points: Vec<V3>,
    pub triangles: Vec<[u32; 3]>,
}

impl Mesh {
    fn clear(&mut self) {
        self.points.clear();
        self.triangles.clear();
    }

    /// A grid of `rows` x `cols` points, already pushed from `first`, joined
    /// into triangles; `wrap` joins the last column to the first.
    fn grid(&mut self, first: usize, rows: usize, cols: usize, wrap: bool) {
        let at = |r: usize, c: usize| (first + r * cols + c % cols) as u32;
        for r in 0..rows - 1 {
            for c in 0..if wrap { cols } else { cols - 1 } {
                let (a, b, d, e) = (at(r, c), at(r, c + 1), at(r + 1, c), at(r + 1, c + 1));
                self.triangles.push([a, b, e]);
                self.triangles.push([a, e, d]);
            }
        }
    }

    /// A fan from a new centre point to a closed ring already pushed.
    fn cap(&mut self, ring: usize, n: usize, centre: V3) {
        let c = self.points.len() as u32;
        self.points.push(centre);
        for k in 0..n {
            self.triangles.push([c, (ring + k) as u32, (ring + (k + 1) % n) as u32]);
        }
    }
}

/// The body's sections, tail to beak: where along it (y), its half-width
/// (x), half-height (z), and how high its middle sits.
const BODY: [(f32, f32, f32, f32); 11] = [
    (-0.2, 0.06, 0.035, 0.0),
    (-0.1, 0.13, 0.085, -0.005),
    (0.02, 0.165, 0.12, -0.015),
    (0.12, 0.14, 0.105, 0.0),
    (0.2, 0.075, 0.075, 0.025),
    (0.24, 0.07, 0.07, 0.04),
    (0.3, 0.092, 0.08, 0.055),
    (0.36, 0.08, 0.07, 0.058),
    (0.4, 0.035, 0.03, 0.05),
    (0.42, 0.016, 0.014, 0.047),
    (0.48, 0.002, 0.002, 0.042),
];

/// Body sections and points around each.
const ALONG: usize = 30;
const AROUND: usize = 14;

fn body_at(y: f32) -> (f32, f32, f32) {
    let k = BODY.iter().position(|s| s.0 >= y).unwrap_or(BODY.len() - 1).max(1);
    let (a, b) = (BODY[k - 1], BODY[k]);
    let u = ((y - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
    let e = u * u * (3.0 - 2.0 * u);
    (a.1 + (b.1 - a.1) * e, a.2 + (b.2 - a.2) * e, a.3 + (b.3 - a.3) * e)
}

/// The right wing's leading edge, shoulder to the hand's tip, with the chord
/// (how far back the wing runs) at each.
const EDGE: [(V3, f32); 4] = [
    ([0.08, 0.1, 0.03], 0.3),
    ([0.22, 0.13, 0.035], 0.31),
    ([0.38, 0.11, 0.04], 0.26),
    ([0.5, 0.05, 0.04], 0.13),
];
const SHOULDER: V3 = EDGE[0].0;

/// Stations along the wing, rows across it; primaries and their length.
const STATIONS: usize = 12;
const ROWS: usize = 4;
const PRIMARIES: usize = 6;

fn edge_at(s: f32) -> (V3, f32) {
    let f = s.clamp(0.0, 1.0) * (EDGE.len() - 1) as f32;
    let k = (f as usize).min(EDGE.len() - 2);
    let u = f - k as f32;
    let (a, b) = (EDGE[k], EDGE[k + 1]);
    (std::array::from_fn(|c| a.0[c] + (b.0[c] - a.0[c]) * u), a.1 + (b.1 - a.1) * u)
}

/// Builds the bird in its own axes, its wings as `w` says.
pub fn build(w: Wings, mesh: &mut Mesh) {
    mesh.clear();

    // The body: rings along it, closed at both ends.
    let first = mesh.points.len();
    let (y0, y1) = (BODY[0].0, BODY[BODY.len() - 1].0);
    for i in 0..ALONG {
        let y = y0 + (y1 - y0) * i as f32 / (ALONG - 1) as f32;
        let (hw, hh, mid) = body_at(y);
        for k in 0..AROUND {
            let a = std::f32::consts::TAU * k as f32 / AROUND as f32;
            mesh.points.push([hw * a.cos(), y, mid + hh * a.sin()]);
        }
    }
    mesh.grid(first, ALONG, AROUND, true);
    mesh.cap(first, AROUND, [0.0, y0 - 0.01, BODY[0].3]);
    mesh.cap(first + (ALONG - 1) * AROUND, AROUND, [0.0, y1 + 0.004, BODY[BODY.len() - 1].3]);

    // The tail: a fan, wider and a little lower at its rounded end.
    let first = mesh.points.len();
    let (along, across) = (5, 7);
    for i in 0..along {
        let u = i as f32 / (along - 1) as f32;
        for j in 0..across {
            let v = 2.0 * j as f32 / (across - 1) as f32 - 1.0;
            mesh.points.push([v * (0.06 + 0.13 * u), -0.15 - 0.3 * u - 0.05 * u * (1.0 - v * v), 0.005 - 0.025 * u]);
        }
    }
    mesh.grid(first, along, across, false);

    for side in [1.0f32, -1.0] {
        let wing = mesh.points.len();
        // The arm and hand: a sheet behind the leading edge, a little arched.
        for i in 0..STATIONS {
            let (lead, chord) = edge_at(i as f32 / (STATIONS - 1) as f32);
            for r in 0..ROWS {
                let v = r as f32 / (ROWS - 1) as f32;
                mesh.points.push([lead[0], lead[1] - chord * v, lead[2] + 0.02 * (std::f32::consts::PI * v).sin()]);
            }
        }
        mesh.grid(wing, STATIONS, ROWS, false);
        // The primaries: from the back of the hand, the outermost pointing
        // out along the wing and each further in pointing further back;
        // folding the wing sweeps them back.
        for p in 0..PRIMARIES {
            let k = p as f32 / (PRIMARIES - 1) as f32;
            let (lead, chord) = edge_at(0.62 + 0.38 * k);
            let root = [lead[0], lead[1] - chord * 0.6, lead[2]];
            let angle = -1.25 + 1.12 * k - 0.6 * w.fold;
            let (dir, across) = ([angle.cos(), angle.sin(), 0.0], [-angle.sin(), angle.cos(), 0.0]);
            let length = 0.26 + 0.1 * k;
            let base = mesh.points.len();
            let segments = 4;
            for s in 0..=segments {
                let u = s as f32 / segments as f32;
                let half = 0.034 * (1.0 - 0.35 * u);
                let along = length * u;
                for side in [-1.0, 1.0] {
                    mesh.points.push([
                        root[0] + dir[0] * along + across[0] * half * side,
                        root[1] + dir[1] * along + across[1] * half * side,
                        root[2] - 0.03 * u * u,
                    ]);
                }
            }
            mesh.grid(base, segments + 1, 2, false);
            let end = mesh.points.len() - 2;
            let tip = [root[0] + dir[0] * (length + 0.025), root[1] + dir[1] * (length + 0.025), root[2] - 0.035];
            mesh.points.push(tip);
            mesh.triangles.push([end as u32, end as u32 + 1, end as u32 + 2]);
        }
        // Drawn in, then raised at the shoulder and bent along the span.
        for p in &mut mesh.points[wing..] {
            let d = (p[0] - SHOULDER[0]) * (1.0 - 0.3 * w.fold);
            let u = (d / 0.85).clamp(0.0, 1.0);
            let a = w.lift + w.bend * u * u;
            let dz = p[2] - SHOULDER[2];
            p[0] = side * (SHOULDER[0] + d * a.cos() - dz * a.sin());
            p[2] = SHOULDER[2] + d * a.sin() + dz * a.cos();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_flat_the_wings_span_what_it_says() {
        let mut m = Mesh::default();
        build(Wings { lift: 0.0, bend: 0.0, fold: 0.0 }, &mut m);
        let (lo, hi) = m.points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p[0]), hi.max(p[0])));
        assert!((hi - lo - SPAN).abs() < 0.05, "span {}", hi - lo);
        assert!((hi + lo).abs() < 1e-4, "not symmetric");
        assert!(m.triangles.iter().flatten().all(|&i| (i as usize) < m.points.len()));
    }

    #[test]
    fn raised_wings_go_up_towards_its_back() {
        let mut m = Mesh::default();
        build(Wings { lift: 0.8, bend: 0.0, fold: 0.0 }, &mut m);
        let top = m.points.iter().map(|p| p[2]).fold(f32::MIN, f32::max);
        assert!(top > 0.4, "the raised wingtips reach only {top}");
    }
}
