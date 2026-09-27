//! The cloth: a sheet of particles draped over an invisible head, simulated
//! with position-based dynamics (Verlet + distance constraints), and the mesh
//! it produces for the renderer.
//!
//! **Topology.** The sheet wraps all the way around the head, like a poncho,
//! not just a flat panel facing the camera - so a turn (`Pose::yaw`) always
//! shows real fabric, never an edge. It is a small fan of rings: one pole
//! vertex at the crown, [`HEAD_RINGS`] rings glued rigidly to the head (the
//! part of the sheet that clings to the skull and shoulders), then
//! [`SKIRT_RINGS`] free rings hanging below - the part this whole card is
//! about. [`COLS`] columns run around the azimuth and wrap.
//!
//! **Kinematic vs free.** The pole and the head rings are *kinematic*: every
//! step they are placed exactly where the head's own rigid transform puts
//! them, with no physics of their own - they are what "the head is what
//! moves" means in code. The skirt rings are *free*: integrated by Verlet in
//! **world space** (not head-local), which is what gives the lag its
//! believability for free - a free particle's `pos - prev` is real
//! accumulated momentum, so when the kinematic collar above it suddenly
//! accelerates, the distance constraint pulls the skirt after it a step
//! late, exactly like a real hem.
//!
//! **Constraints.** Structural (ring-to-ring, round-the-ring), shear
//! (diagonal) and bend (skip-one) distance constraints, solved by a fixed
//! number of Gauss-Seidel passes per physics step - the classic PBD recipe.
//! `sway` softens the correction (looser, more give) and lengthens momentum
//! retention (floatier, slower to settle); it never disables the safety
//! clamps that keep the sheet from exploding.
//!
//! **Rest shape.** The constraint graph's rest lengths are not "flat cloth" -
//! they already describe a bell: the skirt flares wider than the collar
//! (card 326: "a sheet ... draped over ... perhaps soft shoulders") and gets
//! a little low-frequency waviness from the shape's own `humps`, so gravity
//! only has to *settle* the sheet, not invent its silhouette from nothing.

use super::act::Shape;

pub(crate) const COLS: usize = 20;
/// Four rings round the dome, plus a fifth: the neck (see [`build_template`]).
pub(crate) const HEAD_RINGS: usize = 5;
pub(crate) const SKIRT_RINGS: usize = 9;
pub(crate) const RINGS: usize = HEAD_RINGS + SKIRT_RINGS;
/// Pole + every ring.
pub(crate) const VERTS: usize = 1 + RINGS * COLS;
/// The first ring that is simulated rather than glued to the head: rings
/// `0..HEAD_RINGS` are kinematic, so `HEAD_RINGS` itself is the first free
/// one - the ring the skirt actually starts hanging from.
const FIRST_FREE_RING: usize = HEAD_RINGS;

const THETA_TOP: f32 = 0.22; // ~13 degrees off the pole: the crown is not a point on screen either.
/// Where the round part of the head stops - short of the equator, so the
/// dome reads as a ball, not a shape that is still widening when the cloth
/// takes over (card 326 review: "a head sphere that dominates the top").
const THETA_DOME: f32 = 1.15; // ~66 degrees.
/// The neck ring's own height, a little below the dome's widest point.
const THETA_NECK: f32 = 1.35; // ~77 degrees.
/// The neck's radius, as a fraction of the dome's own widest point - the
/// pinch that makes "head, then shoulders" read as two things rather than
/// one continuously widening cone (the review's main complaint).
const NECK_FACTOR: f32 = 0.56;
/// How much wider the hem flares than the neck. Past 1.0 the hem is wider
/// than the head itself, which is what makes it read as a sheet and not a
/// collar - most of this is spent early (see `flare_profile`), so the
/// "shoulders" are already wide just below the neck.
const FLARE: f32 = 1.55;

/// The dome's own widest radius and the neck's radius, both a plain function
/// of `r`: the two numbers `act::Shape` needs (for `margin`/`extent`) without
/// duplicating the rest of the template.
fn dome_and_neck_radius(r: f32) -> (f32, f32) {
    let dome = r * THETA_DOME.sin();
    (dome, dome * NECK_FACTOR)
}

/// The hem's own eventual radius: how far the flare reaches by `t = 1`.
fn hem_radius(r: f32) -> f32 {
    let (_, neck) = dome_and_neck_radius(r);
    neck * (1.0 + FLARE)
}

/// Fast early widening, easing off - the shoulders are most of the way to
/// full width within the first skirt ring or two, not a slow taper all the
/// way to the hem.
fn flare_profile(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t)
}

/// How far this ghost's cloth can reach from its own centreline, at its
/// widest - the hem's flare, almost always, since [`FLARE`] makes it wider
/// than the dome. What an entrance, an exit or a peek has to clear before
/// nothing of the *sheet*, not just the head, is on screen.
pub(crate) fn extent(shape: Shape) -> f32 {
    let r = shape.r.max(0.8);
    let (dome, _) = dome_and_neck_radius(r);
    let hem = hem_radius(r) + shape.hem_amp * 0.74; // the lobes' own bulge, at their biggest.
    dome.max(hem)
}

/// How far the crown sits above the head's own centre - always exactly `r`,
/// whatever the neck pinch does below it.
pub(crate) fn rise_above_centre(shape: Shape) -> f32 {
    shape.r.max(0.8)
}

/// How far the lowest lobe's own drip sits below the head's centre - the
/// neck's own (small) rise above centre subtracted back out, so this matches
/// [`build_template`]'s actual geometry rather than a guess at it.
pub(crate) fn drop_below_centre(shape: Shape) -> f32 {
    let r = shape.r.max(0.8);
    let hem_len = (shape.hem_base + shape.hem_amp).max(r * 0.6);
    let neck_y = r * THETA_NECK.cos();
    (hem_len + shape.hem_amp * 0.6 - neck_y).max(hem_len * 0.5)
}

/// Crown to the lowest a lobe's own drip ever reaches - what a vertical
/// placement has to clear at both ends, matching [`build_template`]'s own
/// geometry rather than a guess at it.
pub(crate) fn total_height(shape: Shape) -> f32 {
    rise_above_centre(shape) + drop_below_centre(shape)
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl V3 {
    pub const ZERO: V3 = V3 { x: 0.0, y: 0.0, z: 0.0 };

    pub fn new(x: f32, y: f32, z: f32) -> V3 {
        V3 { x, y, z }
    }
    pub fn add(self, o: V3) -> V3 {
        V3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
    pub fn sub(self, o: V3) -> V3 {
        V3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
    pub fn scale(self, k: f32) -> V3 {
        V3::new(self.x * k, self.y * k, self.z * k)
    }
    pub fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: V3) -> V3 {
        V3::new(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }
    /// The zero vector normalises to itself rather than NaN - the one place
    /// degenerate geometry (two coincident particles) must not poison a step.
    pub fn normalize(self) -> V3 {
        let l = self.length();
        if l > 1e-6 {
            self.scale(1.0 / l)
        } else {
            V3::ZERO
        }
    }
    /// Rotate around world Y by `yaw` radians - the only rotation a head ever
    /// does here (card 326's "turn").
    pub fn rot_y(self, yaw: f32) -> V3 {
        let (s, c) = yaw.sin_cos();
        V3::new(c * self.x + s * self.z, self.y, -s * self.x + c * self.z)
    }
}

/// The head's rigid placement in world space at one instant.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HeadTarget {
    pub pos: V3,
    pub yaw: f32,
}

impl HeadTarget {
    fn to_world(self, local: V3) -> V3 {
        local.rot_y(self.yaw).add(self.pos)
    }
}

/// One vertex, ready for the GPU: world position, world normal, UV (`u`
/// around the azimuth - front at 0.5 - `v` from crown to hem), and a fold/AO
/// scalar (card 326: "soft shadows in the folds").
#[derive(Clone, Copy, Debug)]
pub(crate) struct RenderVertex {
    pub pos: V3,
    pub normal: V3,
    pub uv: [f32; 2],
    pub fold: f32,
}

/// `az(col)`: azimuth in radians, with `col == COLS/2` (`u == 0.5`) facing
/// world `+Z` - a head at `z < 0` is in front of a camera at the origin
/// looking down `-Z`, so the side facing that camera is the `+Z` side, not
/// `-Z`. (Getting this backwards was a real bug: it put the eyes on the
/// skull's far side, never once visible, however the head turned.) At
/// yaw 0 this is straight at the camera; the seam (`col == 0`) lands at the
/// back, where a viewer never lingers.
fn azimuth(col: usize) -> f32 {
    std::f32::consts::TAU * col as f32 / COLS as f32 - std::f32::consts::FRAC_PI_2
}

fn uv_of(col: usize) -> f32 {
    col as f32 / COLS as f32
}

/// The rest template: every vertex's position in head-local space (head
/// centred at the origin, no rotation), and which ones are kinematic. Also
/// the two per-column jitters (radius and height) that give the hem its own
/// modest waviness before physics does anything at all.
struct Template {
    local: Vec<V3>,
    /// `true` for the pole and every head ring: driven by the head's rigid
    /// transform every step, never by physics.
    kinematic: Vec<bool>,
    ring_of: Vec<usize>,
    col_of: Vec<usize>,
}

fn ring_col_index(ring: usize, col: usize) -> usize {
    1 + ring * COLS + col % COLS
}

fn build_template(shape: Shape) -> Template {
    let r = shape.r.max(0.8);
    let hem_len = (shape.hem_base + shape.hem_amp).max(r * 0.6);
    let humps = shape.humps;

    let mut local = vec![V3::ZERO; VERTS];
    let mut kinematic = vec![false; VERTS];
    let mut ring_of = vec![0usize; VERTS];
    let mut col_of = vec![0usize; VERTS];

    local[0] = V3::new(0.0, r, 0.0);
    kinematic[0] = true;

    // Head rings: the first `HEAD_RINGS - 1` are rigid points on the dome
    // itself, theta running from just off the crown to short of the equator
    // (`THETA_DOME`) - a ball, not a shape still widening when the cloth
    // takes over. The last one is the neck: same rigid, kinematic ring, but
    // pulled in to `NECK_FACTOR` of the dome's own widest radius, which is
    // the pinch that separates "head" from "shoulders" at a glance.
    for ring in 0..HEAD_RINGS - 1 {
        let theta = THETA_TOP + (THETA_DOME - THETA_TOP) * ring as f32 / (HEAD_RINGS - 2) as f32;
        let (radius, y) = (r * theta.sin(), r * theta.cos());
        for col in 0..COLS {
            let i = ring_col_index(ring, col);
            let az = azimuth(col);
            local[i] = V3::new(radius * az.cos(), y, radius * az.sin());
            kinematic[i] = true;
            ring_of[i] = ring;
            col_of[i] = col;
        }
    }
    let neck_ring = HEAD_RINGS - 1;
    let (_, neck_radius) = dome_and_neck_radius(r);
    let neck_y = r * THETA_NECK.cos();
    for col in 0..COLS {
        let i = ring_col_index(neck_ring, col);
        let az = azimuth(col);
        local[i] = V3::new(neck_radius * az.cos(), neck_y, neck_radius * az.sin());
        kinematic[i] = true;
        ring_of[i] = neck_ring;
        col_of[i] = col;
    }

    // Skirt rings: hanging further, flaring wider than the head itself
    // (`FLARE`, most of it spent early - `flare_profile` - so the shoulders
    // are wide just below the neck), with a per-column wave (`humps`) that
    // only ever pulls the hem *down* and *out* at a few spots, never up -
    // "a few distinct lobes/points where the cloth hangs", not a symmetric
    // wobble.
    for j in 1..=SKIRT_RINGS {
        let ring = HEAD_RINGS + j - 1;
        let t = j as f32 / SKIRT_RINGS as f32;
        let base_radius = neck_radius * (1.0 + FLARE * flare_profile(t));
        let base_y = neck_y - hem_len * t;
        for col in 0..COLS {
            let i = ring_col_index(ring, col);
            let az = azimuth(col);
            let lobe = 0.5 + 0.5 * (humps * az + shape.phase0).cos();
            let drip = shape.hem_amp * 0.6 * t * t * lobe;
            let bulge = shape.hem_amp * 0.14 * t * lobe;
            let radius = base_radius + bulge;
            let y = base_y - drip;
            local[i] = V3::new(radius * az.cos(), y, radius * az.sin());
            ring_of[i] = ring;
            col_of[i] = col;
        }
    }

    Template { local, kinematic, ring_of, col_of }
}

#[derive(Clone, Copy)]
struct Edge {
    a: usize,
    b: usize,
    rest: f32,
    /// Structural edges hold shape the most; bend the least. `sway` softens
    /// all of them, but bend is the first thing to go floppy.
    weight: f32,
}

/// The sheet's live state: world-space positions (and their PBD partner,
/// the previous position), plus the fixed template and constraint graph that
/// do not change once a ghost has been cast.
pub(crate) struct Cloth {
    template: Template,
    edges: Vec<Edge>,
    pos: Vec<V3>,
    prev: Vec<V3>,
    fold: Vec<f32>,
    /// Simulated seconds already applied - the fixed-step accumulator's own
    /// clock, so `advance` never depends on how finely it is called (the
    /// house rule every simulation in this codebase follows).
    warped: f64,
}

/// The physics step, seconds - independent of the panel's frame rate.
pub(crate) const PHYS_DT: f32 = 1.0 / 180.0;
const ITERATIONS: usize = 10;
const GRAVITY: f32 = 34.0;
/// Head-sphere collision skin: free particles are kept at least this far
/// outside the head, so the sheet never dips into its own skull.
const SKIN: f32 = 0.35;
/// However loose `sway` is, no particle may move further than this in one
/// physics step - the hard stop behind "no exploding cloth at any setting".
const MAX_STEP: f32 = 6.0;

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn damping(sway: f32) -> f32 {
    lerp(0.958, 0.992, sway.clamp(0.0, 1.0))
}
fn stiffness(sway: f32) -> f32 {
    lerp(0.95, 0.55, sway.clamp(0.0, 1.0))
}

fn add_edge(edges: &mut Vec<Edge>, local: &[V3], a: usize, b: usize, weight: f32) {
    edges.push(Edge { a, b, rest: local[a].sub(local[b]).length(), weight });
}

fn build_edges(template: &Template) -> Vec<Edge> {
    let mut edges = Vec::new();
    let local = &template.local;

    // Pole to the first ring: the crown's own little fan.
    for col in 0..COLS {
        add_edge(&mut edges, local, 0, ring_col_index(0, col), 1.0);
    }

    for ring in 0..RINGS {
        // Round the ring (structural). Skip pure head rings: two rigid
        // points never need a constraint between them.
        if ring >= HEAD_RINGS {
            for col in 0..COLS {
                add_edge(&mut edges, local, ring_col_index(ring, col), ring_col_index(ring, col + 1), 1.0);
            }
        }
        // Down to the next ring (structural), shear (both diagonals) and
        // bend (skip a ring) - but only where at least one endpoint is free,
        // for the same reason.
        if ring + 1 < RINGS && ring + 1 >= FIRST_FREE_RING {
            for col in 0..COLS {
                let (a, b) = (ring_col_index(ring, col), ring_col_index(ring + 1, col));
                add_edge(&mut edges, local, a, b, 1.0);
                let (sa, sb) = (ring_col_index(ring, col), ring_col_index(ring + 1, col + 1));
                add_edge(&mut edges, local, sa, sb, 0.6);
                let (sc, sd) = (ring_col_index(ring, col + 1), ring_col_index(ring + 1, col));
                add_edge(&mut edges, local, sc, sd, 0.6);
            }
        }
        if ring >= HEAD_RINGS && ring + 2 < RINGS {
            for col in 0..COLS {
                add_edge(&mut edges, local, ring_col_index(ring, col), ring_col_index(ring + 2, col), 0.35);
                add_edge(&mut edges, local, ring_col_index(ring, col), ring_col_index(ring, col + 2), 0.35);
            }
        }
    }
    edges
}

impl Cloth {
    /// Cast a fresh sheet over a head held still at `head0`, and settle it
    /// under gravity before anyone can see it (card 326: "warms up
    /// invisibly ... the first frame is a settled sheet, not a falling
    /// one"). `sway` is applied at settle time too, so a very floppy sheet
    /// still starts from a sheet-shaped rest, not a mid-fall one.
    pub(crate) fn spawn(shape: Shape, head0: HeadTarget, sway: f32) -> Cloth {
        let template = build_template(shape);
        let edges = build_edges(&template);
        let pos: Vec<V3> = template.local.iter().map(|&l| head0.to_world(l)).collect();
        let mut cloth = Cloth { template, edges, prev: pos.clone(), pos, fold: vec![0.0; VERTS], warped: 0.0 };
        const SETTLE_STEPS: usize = 420;
        for _ in 0..SETTLE_STEPS {
            cloth.step(head0, PHYS_DT, sway);
        }
        cloth
    }

    /// One fixed-size physics step: place the kinematic part exactly on the
    /// head, Verlet-integrate the free part with gravity and drag, relax the
    /// constraint graph, then push anything that sank into the head back
    /// out.
    fn step(&mut self, head: HeadTarget, dt: f32, sway: f32) {
        let damp = damping(sway);
        let gravity = V3::new(0.0, -GRAVITY, 0.0).scale(dt * dt);
        for i in 0..VERTS {
            if self.template.kinematic[i] {
                self.pos[i] = head.to_world(self.template.local[i]);
                self.prev[i] = self.pos[i];
                continue;
            }
            let vel = self.pos[i].sub(self.prev[i]).scale(damp);
            let mut step = vel.add(gravity);
            let len = step.length();
            if len > MAX_STEP {
                step = step.scale(MAX_STEP / len);
            }
            self.prev[i] = self.pos[i];
            self.pos[i] = self.pos[i].add(step);
        }

        let k = stiffness(sway);
        for _ in 0..ITERATIONS {
            for e in &self.edges {
                if self.template.kinematic[e.a] && self.template.kinematic[e.b] {
                    continue;
                }
                let delta = self.pos[e.b].sub(self.pos[e.a]);
                let dist = delta.length().max(1e-5);
                let diff = (dist - e.rest) / dist * k * e.weight;
                let corr = delta.scale(0.5 * diff);
                if !self.template.kinematic[e.a] {
                    self.pos[e.a] = self.pos[e.a].add(corr);
                }
                if !self.template.kinematic[e.b] {
                    self.pos[e.b] = self.pos[e.b].sub(corr);
                }
            }
            self.collide(head);
        }

        // A last safety net: anything that went non-finite (a pathological
        // parameter combination, not one this design should reach) is
        // snapped back to the head rather than left to poison every frame
        // after it - "no exploding cloth at any param setting" has to hold
        // even if a future edit gets a constant wrong.
        for i in 0..VERTS {
            if !self.pos[i].x.is_finite() || !self.pos[i].y.is_finite() || !self.pos[i].z.is_finite() {
                self.pos[i] = head.to_world(self.template.local[i]);
                self.prev[i] = self.pos[i];
            }
        }
    }

    fn collide(&mut self, head: HeadTarget) {
        // The free skirt starts *below* the neck, not on the head's own
        // dome - so what it must not clip into is the neck's own (much
        // smaller) radius, not the head's full one. Using the head's own
        // radius here was a real bug: every free particle's template
        // position (radius small, `y` near zero, right where the neck pinch
        // put it) measured *less* than that radius from the head's centre,
        // so the very first settle pushed the entire skirt up and out to
        // sit on the head's own equator instead of hanging from the neck.
        let head_r = self.template.local[0].y; // the head sphere's own radius (apex's own `y`).
        let (_, neck_r) = dome_and_neck_radius(head_r);
        let min_r = (neck_r + SKIN).max(0.05);
        for i in 0..VERTS {
            if self.template.kinematic[i] {
                continue;
            }
            let rel = self.pos[i].sub(head.pos).rot_y(-head.yaw);
            let d = rel.length();
            if d < min_r {
                let out = if d > 1e-5 { rel.scale(min_r / d) } else { V3::new(0.0, min_r, 0.0) };
                self.pos[i] = out.rot_y(head.yaw).add(head.pos);
            }
        }
    }

    /// Advance from wherever this sheet's own clock is to `target` seconds,
    /// at the fixed [`PHYS_DT`], asking `head_at` for the head's rigid pose
    /// at each step's own absolute time. The step count comes from total
    /// simulated time, not a counter, so two callers stepping by different
    /// amounts still agree (card 326's rule, and `flock::Flock::advance`'s).
    pub(crate) fn advance(&mut self, target: f64, sway: f32, mut head_at: impl FnMut(f64) -> HeadTarget) {
        const CATCHUP: i64 = 240; // at most 1.3s of steps in one call, however far `target` jumped.
        let dt = f64::from(PHYS_DT);
        let already = (self.warped / dt).round() as i64;
        let want = (target / dt + 1e-6).floor() as i64;
        let from = already.max(want - CATCHUP);
        for s in from..want {
            let t = (s + 1) as f64 * dt;
            self.step(head_at(t), PHYS_DT, sway);
        }
        self.warped = want.max(already) as f64 * dt;
    }

    /// The mesh for the renderer: every vertex's world position, a normal
    /// from its *actual* current neighbours (so a fold really does change
    /// how it catches the light), UV, and the fold/AO term.
    pub(crate) fn render_vertices(&mut self) -> Vec<RenderVertex> {
        self.compute_fold();
        (0..VERTS)
            .map(|i| {
                let ring = if i == 0 { 0 } else { self.template.ring_of[i] + 1 };
                let col = if i == 0 { 0 } else { self.template.col_of[i] };
                let normal = self.normal_at(i);
                RenderVertex {
                    pos: self.pos[i],
                    normal,
                    uv: [uv_of(col), ring as f32 / RINGS as f32],
                    fold: self.fold[i],
                }
            })
            .collect()
    }

    fn neighbour(&self, i: usize, ring: i64, col_delta: i64) -> Option<usize> {
        if i == 0 {
            return None;
        }
        let r = self.template.ring_of[i] as i64 + ring;
        if !(0..RINGS as i64).contains(&r) {
            return None;
        }
        let c = (self.template.col_of[i] as i64 + col_delta).rem_euclid(COLS as i64) as usize;
        Some(ring_col_index(r as usize, c))
    }

    fn normal_at(&self, i: usize) -> V3 {
        let p = self.pos[i];
        let right = self.neighbour(i, 0, 1).map(|j| self.pos[j].sub(p)).unwrap_or(V3::new(1.0, 0.0, 0.0));
        let left = self.neighbour(i, 0, -1).map(|j| self.pos[j].sub(p)).unwrap_or(right.scale(-1.0));
        let down = self.neighbour(i, 1, 0).map(|j| self.pos[j].sub(p));
        let up = self.neighbour(i, -1, 0).map(|j| self.pos[j].sub(p));
        let vertical = match (up, down) {
            (Some(u), Some(d)) => d.sub(u),
            (Some(u), None) => u.scale(-1.0),
            (None, Some(d)) => d,
            (None, None) => V3::new(0.0, -1.0, 0.0),
        };
        let horizontal = right.sub(left);
        // `horizontal x vertical`, not the other way round: `vertical x
        // horizontal` pointed inward, toward the head's own centre, on every
        // vertex checked by hand (a real bug - it meant every fold's light
        // and dark side was reversed) - verified against the fallback below,
        // which is unambiguously outward.
        let n = horizontal.cross(vertical).normalize();
        // The apex's own outward normal (up the crown) is what the fan
        // above it should shade like, not whatever the first ring's tangent
        // frame happens to compute.
        if i == 0 {
            V3::new(0.0, 1.0, 0.0)
        } else if n.length() > 0.5 {
            n
        } else {
            self.pos[i].sub(V3::ZERO).normalize()
        }
    }

    /// A cheap curvature proxy: how far a vertex sits behind the plane its
    /// neighbours describe, along its own normal. Negative (a valley) darkens;
    /// convex points are left alone.
    fn compute_fold(&mut self) {
        for i in 1..VERTS {
            let mut acc = V3::ZERO;
            let mut n = 0.0;
            for (dr, dc) in [(0, 1), (0, -1), (1, 0), (-1, 0)] {
                if let Some(j) = self.neighbour(i, dr, dc) {
                    acc = acc.add(self.pos[j]);
                    n += 1.0;
                }
            }
            if n < 2.0 {
                self.fold[i] = 0.0;
                continue;
            }
            let avg = acc.scale(1.0 / n);
            let laplacian = self.pos[i].sub(avg);
            let normal = self.normal_at(i);
            self.fold[i] = laplacian.dot(normal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patches::ghosts::act::Shape;
    use crate::rng::Rng;

    fn shape(rng: &mut Rng) -> Shape {
        Shape::new(rng, 12.0)
    }

    fn head(pos: V3, yaw: f32) -> HeadTarget {
        HeadTarget { pos, yaw }
    }

    /// The mesh's own normals point outward, away from the head's centre -
    /// checked at a ring far enough from the pole that "outward" is
    /// unambiguous - not inward (the bug this test is named for: a swapped
    /// cross-product order once made every fold's light and dark side
    /// backwards).
    #[test]
    fn normals_point_outward_not_inward() {
        let mut rng = Rng::new(11);
        let s = shape(&mut rng);
        let mut cloth = Cloth::spawn(s, head(V3::ZERO, 0.0), 0.45);
        let verts = cloth.render_vertices();
        for col in 0..COLS {
            let v = &verts[ring_col_index(2, col)];
            let outward = v.pos.sub(V3::new(0.0, v.pos.y, 0.0)).normalize();
            assert!(v.normal.dot(outward) > 0.5, "ring 2 col {col}: normal {:?} vs outward {outward:?}", v.normal);
        }
    }

    /// A freshly settled sheet, over a head that never moves, has every
    /// vertex finite and within a sane distance of the head - the first
    /// "does not explode" check, before anything is asked to move at all.
    #[test]
    fn settling_never_explodes() {
        let mut rng = Rng::new(11);
        for _ in 0..6 {
            let s = shape(&mut rng);
            let cloth = Cloth::spawn(s, head(V3::ZERO, 0.0), 0.5);
            for p in &cloth.pos {
                assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
                assert!(p.sub(V3::ZERO).length() < s.total_height() * 4.0 + 20.0, "{p:?} flew away for r={}", s.r);
            }
        }
    }

    /// The full `sway` range, and a wide range of shapes, all settle to
    /// something finite and bounded - not just the default.
    #[test]
    fn every_sway_setting_is_stable() {
        let mut rng = Rng::new(5);
        for sway in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let s = shape(&mut rng);
            let cloth = Cloth::spawn(s, head(V3::new(3.0, -2.0, 1.0), 0.4), sway);
            for p in &cloth.pos {
                assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite(), "sway {sway}");
            }
        }
    }

    /// A moving, turning head over several seconds of real motion never
    /// blows the sheet up either - the case the settle-only test above does
    /// not reach.
    #[test]
    fn moving_head_stays_stable() {
        let mut rng = Rng::new(21);
        let s = shape(&mut rng);
        let mut cloth = Cloth::spawn(s, head(V3::ZERO, 0.0), 0.9);
        let mut t = 0.0_f64;
        while t < 8.0 {
            t += 1.0 / 30.0;
            let tt = t;
            cloth.advance(t, 0.9, |sub| {
                head(V3::new(20.0 * (tt as f32 * 0.5).sin(), 2.0 * (tt as f32).cos(), 0.0), (sub as f32 * 0.3).sin())
            });
        }
        for p in &cloth.pos {
            assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
            assert!(p.length() < 200.0, "{p:?} escaped");
        }
    }

    /// Stepping to the same target time by different call granularities
    /// gives the same sheet - the determinism every fixed-step sim here
    /// promises.
    #[test]
    fn advance_does_not_depend_on_call_granularity() {
        let mut rng = Rng::new(3);
        let s = shape(&mut rng);
        let head_at = |t: f64| head(V3::new(t as f32, 0.0, 0.0), 0.0);

        // Unlike `act`'s own step-size test - which can compare `poses_at`
        // at one fixed instant regardless of how far a loop overshot it,
        // because a `Pose` is a stateless function of `t` - `Cloth::advance`
        // is stateful and cannot be asked about a moment it has already
        // stepped past. So both loops are driven by step sizes that divide
        // the target evenly (60 steps of 1/30s, 90 of 1/45s), landing on
        // exactly 2.0 with no overshoot for either.
        let mut coarse = Cloth::spawn(s, head_at(0.0), 0.4);
        for i in 1..=60 {
            let t = i as f64 / 30.0;
            coarse.advance(t, 0.4, &head_at);
        }

        let mut fine = Cloth::spawn(s, head_at(0.0), 0.4);
        for i in 1..=90 {
            let t = i as f64 / 45.0;
            fine.advance(t, 0.4, &head_at);
        }

        for (a, b) in coarse.pos.iter().zip(fine.pos.iter()) {
            assert!(a.sub(*b).length() < 1e-4, "{a:?} vs {b:?}");
        }
    }
}
