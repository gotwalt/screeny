// Hearth: a 3D Eulerian fire, stepped and ray-marched in compute.
//
// The grid is x across the panel, y up, z into the hearth. Every field lives
// in a storage buffer, cell (x, y, z) at (z * GH + y) * GW + x. Velocity and
// the scalars are vec4s so one trilinear read serves several fields:
//
//   vel  = (u, v, w, -)
//   scal = (temperature 0..1, fuel, smoke, expansion source)
//
// The step, entry point by entry point, in the order `gpu.rs` dispatches it:
//
//   adv_*       MacCormack advection (Selle et al. 2008): a forward
//               semi-Lagrangian step, the same step run backwards from its
//               result, half the round trip's error taken off, clamped to
//               the cells the forward step read so it cannot ring. Results in
//               the *_next buffers; Rust copies them back.
//   combust     fuel held at the logs' level, burnt at a fixed rate; gas held
//               as hot as its fuel, else cooling (theta^4 + linear); burnt fuel
//               leaves smoke and an expansion source (Nguyen, Fedkiw, Jensen
//               2002). Then buoyancy, smoke weight, the wind, drag.
//   curl_of, confine   vorticity confinement (Fedkiw, Stam, Jensen 2001).
//   divergence, pres_red, pres_black, gradient   the projection:
//               red-black Gauss-Seidel with over-relaxation, a fixed number
//               of sweeps, warm-started from the last step's pressure.
//   embers      the sparks, carried by the velocity field.
//   light_*     the fire's own light, gathered onto a coarse grid and spread,
//               for the smoke to be lit by.
//   march       one invocation per LED: ss x ss rays through the volume,
//               emission by the blackbody table, smoke absorbing it and
//               scattering the fire's light.
//
// Boundaries: open at the top, the sides, front and back (pressure 0 beyond,
// values carried out by clamped sampling); the hearth floor is a wall.

struct Params {
  dims: vec4<u32>,     // GW, GH, GD, step number
  coarse: vec4<u32>,   // CW, CH, CD, ember slots
  a: vec4<f32>,        // dt, buoyancy, smoke_weight, drag
  b: vec4<f32>,        // wind_grip, confine, burn, fuel_heat
  c: vec4<f32>,        // cool4, cool1, expand, smoke_yield
  d: vec4<f32>,        // smoke_fade, smoke_absorb, smoke_light, emission
  eye: vec4<f32>,      // camera position; w: samples per axis
  right: vec4<f32>,    // camera right * tan(half width)
  up: vec4<f32>,       // camera up * tan(half height)
  fwd: vec4<f32>,      // camera forward; w: march step, cells
  stove: vec4<f32>,    // draft (0: an open fire), ceiling row, flue radius, -
  logs: vec4<f32>,     // rows the woodpile covers, -, -, -
  wind: array<vec4<f32>, 40>,  // per row, four rows to a vec4
  clock: vec4<f32>,    // the time's depth slab (from, to), which showing (for its noise), -
  glyph: array<vec4<u32>, 16>, // the time on the panel, a bit per LED, 2 words to a row
  moving: array<vec4<u32>, 16>, // the pieces of it in motion, the same way
  numerals: vec4<f32>, // readiness to catch, fuel held, gas time, jet top row
  jet: vec4<f32>,      // the jet that lights them: fuel (0: none), lift, columns from, to
};

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> vel: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> scal: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> vel_next: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> scal_next: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> curl: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read_write> scal_back: array<vec4<f32>>;
@group(0) @binding(7) var<storage, read_write> div: array<f32>;
@group(0) @binding(8) var<storage, read_write> pres: array<f32>;
// The woodpile, its own rows of the grid: (fuel level, glow, 1 inside a
// log, -), cell (x, y, z) at (z * rows + y) * GW + x.
@group(0) @binding(9) var<storage, read> bed: array<vec4<f32>>;
@group(0) @binding(10) var<storage, read_write> light: array<f32>;
@group(0) @binding(11) var<storage, read_write> light2: array<f32>;
@group(0) @binding(12) var<storage, read_write> parts: array<vec4<f32>>;
@group(0) @binding(13) var<storage, read_write> out: array<vec4<f32>>;
@group(0) @binding(14) var<uniform> lut: array<vec4<f32>, 512>;
// How far each cell of the numerals has caught (2026-10-26), 0 to 1.
@group(0) @binding(15) var<storage, read_write> caught: array<f32>;

const SOR: f32 = 1.8;
// How fast a cell of a numeral catches once it is hot enough: per second
// of gas time, so a quarter of a second to take fully.
const CATCH_RATE: f32 = 4.0;
const EMBER_OUT: f32 = 0.30;
// A spark's bounce (card 376): the share of its speed into a wall it keeps,
// the share of its slide along it, and the impact speed (design cells per
// second) that counts as a bounce rather than a touch.
const EMBER_REST: f32 = 0.3;
const EMBER_SLIDE: f32 = 0.6;
const EMBER_IMPACT: f32 = 26.0;
const LIGHT_CELL: u32 = 4u;
const LIGHT_RADIUS: i32 = 6;
// Upward acceleration in the flue at full draft, cells per second squared.
const DRAFT_PULL: f32 = 400.0;
// The draft's pull toward the flue, at full draft, cells per second
// squared: strongest under the ceiling, a gentle lean lower down.
const DRAFT_DRAW: f32 = 60.0;
// How far under the ceiling the draw is at its strongest, rows.
const DRAFT_POOL: f32 = 12.0;

fn gw() -> u32 { return P.dims.x; }
fn gh() -> u32 { return P.dims.y; }
fn gd() -> u32 { return P.dims.z; }
fn idx(x: u32, y: u32, z: u32) -> u32 { return (z * gh() + y) * gw() + x; }
fn inside(g: vec3<u32>) -> bool { return g.x < gw() && g.y < gh() && g.z < gd(); }
fn wind_at(y: u32) -> f32 { return P.wind[y / 4u][y % 4u]; }

// ---------------------------------------------------------------- the firebox (card 373)
//
// With a draft, the volume is the inside of a wood stove: the row at
// `stove.y` and above is the firebox ceiling, solid except for a round flue
// in the middle that runs up out of the top of the grid. With no draft it is
// card 372's open fire.
//
// The sides stay open, and the chimney's draw is a force, not a pressure.
// A closed box with the flue as its only outlet is the honest model, and it
// was tried: the fixed number of pressure sweeps cannot carry the flue's low
// pressure down through 130 rows of box, the projection then holds every
// plume to the logs, and the fire smothers (card 373's Log). So the flue
// pulls the gas up, the gas under the ceiling is drawn in toward the flue,
// and lower down the tongues lean in a little; the open sides, off-screen,
// stand in for the air intakes.

fn stove_on() -> bool { return P.stove.x > 0.0; }

fn bed_rows() -> u32 { return u32(P.logs.x); }

fn in_log(x: i32, y: i32, z: i32) -> bool {
  if (x < 0 || z < 0 || y < 0 || x >= i32(gw()) || z >= i32(gd()) || y >= i32(bed_rows())) { return false; }
  return bed[(u32(z) * bed_rows() + u32(y)) * gw() + u32(x)].z > 0.5;
}

fn in_flue(x: f32, z: f32) -> bool {
  let d = vec2<f32>(x - 0.5 * f32(gw() - 1u), z - 0.5 * f32(gd() - 1u));
  return dot(d, d) < P.stove.z * P.stove.z;
}

// The time (2026-10-26): its shape on the panel, carried back along the
// camera's rays through a slab of the depth. 0 off it, 1 on it, 2 on a
// piece of it in motion.
fn in_clock(x: i32, y: i32, z: i32) -> u32 {
  let zf = f32(z);
  if (zf < P.clock.x || zf > P.clock.y) { return 0u; }
  let d = vec3<f32>(f32(x), f32(y), zf) - P.eye.xyz;
  let depth = dot(d, P.fwd.xyz);
  if (depth < 1.0) { return 0u; }
  // As `march` casts them: dir = fwd + u * right + v * up.
  let u = dot(d, P.right.xyz) / (depth * dot(P.right.xyz, P.right.xyz));
  let v = dot(d, P.up.xyz) / (depth * dot(P.up.xyz, P.up.xyz));
  let lx = i32(floor((u + 1.0) * 32.0));
  let ly = i32(floor((1.0 - v) * 16.0));
  if (lx < 0 || ly < 0 || lx >= 64 || ly >= 32) { return 0u; }
  let w = u32(ly) * 2u + u32(lx) / 32u;
  let bit = u32(lx) % 32u;
  if (((P.moving[w / 4u][w % 4u] >> bit) & 1u) == 1u) { return 2u; }
  return (P.glyph[w / 4u][w % 4u] >> bit) & 1u;
}

// Is grid cell (x, y, z) - possibly outside the grid - solid?
fn solid(x: i32, y: i32, z: i32) -> bool {
  if (y < 0) { return true; }
  if (in_log(x, y, z)) { return true; }
  if (!stove_on()) { return false; }
  if (x < 0 || z < 0 || x >= i32(gw()) || z >= i32(gd())) { return false; }
  return f32(y) >= P.stove.y && !in_flue(f32(x), f32(z));
}

// A sample position kept out of the ceiling: what a trace that would have
// read from inside it reads instead is the gas just under it.
fn keep_in(p: vec3<f32>) -> vec3<f32> {
  if (!stove_on() || in_flue(p.x, p.z)) { return p; }
  return vec3<f32>(p.x, min(p.y, P.stove.y - 1.0), p.z);
}

// No gas moves faster than this, cells per second: a guard against the
// rare runaway (an unbounded confinement kick feeding itself), which once
// left a whole fire frozen in its logs for seconds. Real flames here move
// at a third of it. Times the step (in grid cells, k = 2/3) it is 2.9 cells:
// under the 3 cells a semi-Lagrangian trace may cross (card 375).
const VMAX: f32 = 130.0;

fn tame(v: vec3<f32>) -> vec3<f32> {
  let m = VMAX * P.stove.w;
  return clamp(v, vec3<f32>(-m), vec3<f32>(m));
}

fn emit(theta: f32) -> vec3<f32> {
  return lut[u32(clamp(theta, 0.0, 1.0) * 511.0 + 0.5)].rgb;
}

fn hash(n: u32) -> u32 {
  var x = n;
  x ^= x >> 16u; x *= 0x7feb352du;
  x ^= x >> 15u; x *= 0x846ca68bu;
  x ^= x >> 16u;
  return x;
}

fn rand(n: u32) -> f32 { return f32(hash(n) >> 8u) / 16777216.0; }

// Smooth value noise in [0, 1), for the numerals' tongues.
fn lattice(i: vec3<i32>) -> f32 {
  return rand(u32(i.x) * 73856093u ^ u32(i.y) * 19349663u ^ u32(i.z) * 83492791u);
}
fn vnoise(p: vec3<f32>) -> f32 {
  let i = vec3<i32>(floor(p));
  let f = p - floor(p);
  let u = f * f * (3.0 - 2.0 * f);
  let x00 = mix(lattice(i), lattice(i + vec3<i32>(1, 0, 0)), u.x);
  let x10 = mix(lattice(i + vec3<i32>(0, 1, 0)), lattice(i + vec3<i32>(1, 1, 0)), u.x);
  let x01 = mix(lattice(i + vec3<i32>(0, 0, 1)), lattice(i + vec3<i32>(1, 0, 1)), u.x);
  let x11 = mix(lattice(i + vec3<i32>(0, 1, 1)), lattice(i + vec3<i32>(1, 1, 1)), u.x);
  return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

// The numerals as fuel (2026-10-26, "numerals are the fuel"): how much
// cell `i` holds, broken into tongues rising in gas time; 0 off the
// numerals.
//
// A cell of a numeral holds nothing until it has caught (the owner: "when
// the clock comes up it looks animated as opposed to a weird feature of
// the fire itself"). It catches where the fire's own hot gas reaches it -
// the flames, the jet, the numerals already alight below it - and stays
// caught while it is part of the time. `numerals.x`, the readiness, lowers
// the heat it takes as the time comes up, through drifting noise, so that
// in the end all of it catches, in a different order every time. Pieces in
// motion burn without catching: they move through cells too fast to.
fn numeral_fuel(x: i32, y: i32, z: i32, i: u32, temp: f32, dt: f32) -> f32 {
  let on = in_clock(x, y, z);
  if (on == 0u || P.numerals.y <= 0.0) {
    caught[i] = 0.0;
    return 0.0;
  }
  let s = 3.0 * P.stove.w / (2.0 / 3.0);
  let t = P.numerals.z;
  var c = 1.0;
  if (on == 1u) {
    c = caught[i];
    if (c < 1.0) {
      let q = vec3<f32>(f32(x) / (1.5 * s) + P.clock.z, f32(y) / (1.5 * s) - 0.7 * t, f32(z) / (1.5 * s) + 17.0);
      if (2.5 * temp + 1.3 * P.numerals.x + 0.7 * vnoise(q) > 1.2) {
        c = min(1.0, c + dt * CATCH_RATE);
      }
      caught[i] = c;
    }
  }
  let p = vec3<f32>(f32(x) / s, f32(y) / s - 1.6 * t, f32(z) / s + 0.4 * t);
  let tongues = clamp(1.6 * vnoise(p) - 0.3, 0.0, 1.0);
  return P.numerals.y * tongues * c;
}

// ---------------------------------------------------------------- sampling

struct Tap { i: u32, f: vec3<f32> };

fn tap(p: vec3<f32>) -> Tap {
  let hi = vec3<f32>(f32(gw() - 1u), f32(gh() - 1u), f32(gd() - 1u));
  let q = clamp(p, vec3<f32>(0.0), hi);
  let c = min(vec3<u32>(q), vec3<u32>(gw() - 2u, gh() - 2u, gd() - 2u));
  return Tap(idx(c.x, c.y, c.z), q - vec3<f32>(c));
}

fn corner(t: Tap, k: u32) -> u32 {
  return t.i + select(0u, 1u, (k & 1u) != 0u) + select(0u, gw(), (k & 2u) != 0u) + select(0u, gw() * gh(), (k & 4u) != 0u);
}

fn weight(t: Tap, k: u32) -> f32 {
  let wx = select(1.0 - t.f.x, t.f.x, (k & 1u) != 0u);
  let wy = select(1.0 - t.f.y, t.f.y, (k & 2u) != 0u);
  let wz = select(1.0 - t.f.z, t.f.z, (k & 4u) != 0u);
  return wx * wy * wz;
}

fn tri_vel(t: Tap) -> vec4<f32> {
  var s = vec4<f32>(0.0);
  for (var k = 0u; k < 8u; k++) { s += weight(t, k) * vel[corner(t, k)]; }
  return s;
}

fn tri_scal(t: Tap) -> vec4<f32> {
  var s = vec4<f32>(0.0);
  for (var k = 0u; k < 8u; k++) { s += weight(t, k) * scal[corner(t, k)]; }
  return s;
}

fn tri_vel_next(t: Tap) -> vec4<f32> {
  var s = vec4<f32>(0.0);
  for (var k = 0u; k < 8u; k++) { s += weight(t, k) * vel_next[corner(t, k)]; }
  return s;
}

fn tri_scal_next(t: Tap) -> vec4<f32> {
  var s = vec4<f32>(0.0);
  for (var k = 0u; k < 8u; k++) { s += weight(t, k) * scal_next[corner(t, k)]; }
  return s;
}

// Where the forward step at cell p read from: a midpoint trace.
fn source_of(p: vec3<f32>, i: u32) -> vec3<f32> {
  let dt = P.a.x;
  let mid = keep_in(p - 0.5 * dt * vel[i].xyz);
  return keep_in(p - dt * tri_vel(tap(mid)).xyz);
}

// And where the backward step from p reads: the same trace, the other way.
fn ahead_of(p: vec3<f32>, i: u32) -> vec3<f32> {
  let dt = P.a.x;
  let mid = keep_in(p + 0.5 * dt * vel[i].xyz);
  return keep_in(p + dt * tri_vel(tap(mid)).xyz);
}

// ---------------------------------------------------------------- advection

@compute @workgroup_size(4, 4, 4)
fn adv_fwd(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let i = idx(g.x, g.y, g.z);
  let t = tap(source_of(vec3<f32>(g), i));
  vel_next[i] = tri_vel(t);
  scal_next[i] = tri_scal(t);
}

@compute @workgroup_size(4, 4, 4)
fn adv_back_vel(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let i = idx(g.x, g.y, g.z);
  curl[i] = tri_vel_next(tap(ahead_of(vec3<f32>(g), i)));
}

@compute @workgroup_size(4, 4, 4)
fn adv_back_scal(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let i = idx(g.x, g.y, g.z);
  scal_back[i] = tri_scal_next(tap(ahead_of(vec3<f32>(g), i)));
}

@compute @workgroup_size(4, 4, 4)
fn adv_fix_vel(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let i = idx(g.x, g.y, g.z);
  let t = tap(source_of(vec3<f32>(g), i));
  var lo = vel[t.i];
  var hi = lo;
  for (var k = 1u; k < 8u; k++) { let v = vel[corner(t, k)]; lo = min(lo, v); hi = max(hi, v); }
  vel_next[i] = clamp(vel_next[i] + 0.5 * (vel[i] - curl[i]), lo, hi);
}

@compute @workgroup_size(4, 4, 4)
fn adv_fix_scal(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let i = idx(g.x, g.y, g.z);
  let t = tap(source_of(vec3<f32>(g), i));
  var lo = scal[t.i];
  var hi = lo;
  for (var k = 1u; k < 8u; k++) { let v = scal[corner(t, k)]; lo = min(lo, v); hi = max(hi, v); }
  scal_next[i] = clamp(scal_next[i] + 0.5 * (scal[i] - scal_back[i]), lo, hi);
}

// ---------------------------------------------------------------- combustion and forces

@compute @workgroup_size(4, 4, 4)
fn combust(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let i = idx(g.x, g.y, g.z);
  let dt = P.a.x;
  var b = vec4<f32>(0.0);
  if (g.y < bed_rows()) { b = bed[(g.z * bed_rows() + g.y) * gw() + g.x]; }
  if (solid(i32(g.x), i32(g.y), i32(g.z))) {
    vel[i] = vec4<f32>(0.0);
    // Inside a log: no gas, only the log's own glow.
    scal[i] = vec4<f32>(b.y, 0.0, 0.0, 0.0);
    return;
  }
  var s = scal[i];
  // The woodpile (exp/hearth-speed): fuel held at its level in the shell
  // round each log, the coals' glow under it.
  var temp = max(s.x, b.y);
  var fuel = max(max(s.y, b.x), numeral_fuel(i32(g.x), i32(g.y), i32(g.z), i, s.x, dt));
  // The jet (2026-10-26): a burst of gas from the bed up to the numerals'
  // foot, in tongues, pushed up hard - what lights them.
  // Its footprint is round, as wide across as `jet.z..jet.w` and as deep
  // as the firebox allows, and thins toward its edge.
  let jet_r = 0.5 * (P.jet.w - P.jet.z);
  let jet_d = vec2<f32>(
    (f32(g.x) - 0.5 * (P.jet.z + P.jet.w)) / max(jet_r, 1.0),
    (f32(g.z) - 0.5 * f32(gd() - 1u)) / max(min(jet_r, 0.4 * f32(gd())), 1.0));
  let jet_edge = 1.0 - dot(jet_d, jet_d);
  let jetting = P.jet.x > 0.0 && jet_edge > 0.0 && f32(g.y) <= P.numerals.w;
  if (jetting) {
    let s3 = 3.0 * P.stove.w / (2.0 / 3.0);
    let q = vec3<f32>(f32(g.x) / s3, f32(g.y) / s3 - 3.0 * P.numerals.z, f32(g.z) / s3);
    fuel = max(fuel, P.jet.x * sqrt(jet_edge) * clamp(1.8 * vnoise(q) - 0.4, 0.0, 1.0));
  }
  let burnt = fuel * (1.0 - exp(-P.b.z * dt));
  let left = fuel - burnt;
  let cooled = temp - dt * (P.c.x * temp * temp * temp * temp + P.c.y * temp);
  temp = clamp(max(cooled, min(1.0, fuel * P.b.w)), 0.0, 1.0);
  let smoke = min(s.z * exp(-P.d.x * dt) + burnt * P.c.w, 2.0);
  scal[i] = vec4<f32>(temp, left, smoke, P.c.z * burnt / dt);

  var v = vel[i].xyz;
  v.y += dt * (P.a.y * temp - P.a.z * smoke);
  if (jetting) { v.y += dt * P.jet.y * jet_edge; }
  v.x += (wind_at(g.y) - v.x) * (1.0 - exp(-P.b.x * dt));
  v *= exp(-P.a.w * dt);
  if (g.y == 0u) { v.y = max(v.y, 0.0); }
  // The chimney pulls: in the flue, and just under its mouth, the gas is
  // drawn up.
  if (stove_on()) {
    let x = f32(g.x);
    let y = f32(g.y);
    let z = f32(g.z);
    if (y >= P.stove.y - 3.0 && in_flue(x, z)) {
      v.y += dt * DRAFT_PULL * P.stove.w * P.stove.x;
    } else {
      // Toward the flue's axis, across and in depth.
      let to = vec2<f32>(0.5 * f32(gw() - 1u) - x, 0.5 * f32(gd() - 1u) - z);
      let dir = to / (length(to) + 1.0);
      let k = 0.35 + 0.65 * smoothstep(P.stove.y - DRAFT_POOL * P.stove.w, P.stove.y - 2.0, y);
      v.x += dt * DRAFT_DRAW * P.stove.w * P.stove.x * k * dir.x;
      v.z += dt * DRAFT_DRAW * P.stove.w * P.stove.x * k * dir.y;
    }
  }
  v = tame(v);
  vel[i] = vec4<f32>(v, 0.0);
}

// ---------------------------------------------------------------- vorticity confinement

fn vel_c(x: i32, y: i32, z: i32) -> vec3<f32> {
  let c = clamp(vec3<i32>(x, y, z), vec3<i32>(0), vec3<i32>(i32(gw()) - 1, i32(gh()) - 1, i32(gd()) - 1));
  return vel[idx(u32(c.x), u32(c.y), u32(c.z))].xyz;
}

@compute @workgroup_size(4, 4, 4)
fn curl_of(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let p = vec3<i32>(g);
  let dx = 0.5 * (vel_c(p.x + 1, p.y, p.z) - vel_c(p.x - 1, p.y, p.z));
  let dy = 0.5 * (vel_c(p.x, p.y + 1, p.z) - vel_c(p.x, p.y - 1, p.z));
  let dz = 0.5 * (vel_c(p.x, p.y, p.z + 1) - vel_c(p.x, p.y, p.z - 1));
  let w = vec3<f32>(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x);
  curl[idx(g.x, g.y, g.z)] = vec4<f32>(w, length(w));
}

fn curl_mag(x: i32, y: i32, z: i32) -> f32 {
  let c = clamp(vec3<i32>(x, y, z), vec3<i32>(0), vec3<i32>(i32(gw()) - 1, i32(gh()) - 1, i32(gd()) - 1));
  return curl[idx(u32(c.x), u32(c.y), u32(c.z))].w;
}

@compute @workgroup_size(4, 4, 4)
fn confine(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let p = vec3<i32>(g);
  let eta = 0.5 * vec3<f32>(
    curl_mag(p.x + 1, p.y, p.z) - curl_mag(p.x - 1, p.y, p.z),
    curl_mag(p.x, p.y + 1, p.z) - curl_mag(p.x, p.y - 1, p.z),
    curl_mag(p.x, p.y, p.z + 1) - curl_mag(p.x, p.y, p.z - 1));
  let n = eta / (length(eta) + 1e-5);
  let i = idx(g.x, g.y, g.z);
  let f = P.b.y * cross(n, curl[i].xyz);
  vel[i] = vec4<f32>(vel[i].xyz + P.a.x * f, 0.0);
}

// ---------------------------------------------------------------- projection

@compute @workgroup_size(4, 4, 4)
fn divergence(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let p = vec3<i32>(g);
  let i = idx(g.x, g.y, g.z);
  if (solid(p.x, p.y, p.z)) {
    div[i] = 0.0;
    return;
  }
  let d = 0.5 * (vel_w(p.x + 1, p.y, p.z).x - vel_w(p.x - 1, p.y, p.z).x
               + vel_w(p.x, p.y + 1, p.z).y - vel_w(p.x, p.y - 1, p.z).y
               + vel_w(p.x, p.y, p.z + 1).z - vel_w(p.x, p.y, p.z - 1).z);
  div[i] = d - scal[i].w;
}

// The velocity a neighbour shows a cell: nothing through a wall, the
// floor or the ceiling.
fn vel_w(x: i32, y: i32, z: i32) -> vec3<f32> {
  if (solid(x, y, z)) { return vec3<f32>(0.0); }
  return vel_c(x, y, z);
}

// Pressure beyond the open faces is 0; across a wall, the floor or the
// ceiling it mirrors.
fn pres_at(x: i32, y: i32, z: i32, own: f32) -> f32 {
  if (solid(x, y, z)) { return own; }
  if (x < 0 || z < 0 || x >= i32(gw()) || y >= i32(gh()) || z >= i32(gd())) { return 0.0; }
  return pres[idx(u32(x), u32(y), u32(z))];
}

// Dispatched over half the cells across: invocation gx is the cell of this
// colour in the pair (2 gx, 2 gx + 1).
fn relax(h: vec3<u32>, colour: u32) {
  let x = 2u * h.x + ((h.y + h.z + colour) & 1u);
  let g = vec3<u32>(x, h.y, h.z);
  if (!inside(g)) { return; }
  let i = idx(g.x, g.y, g.z);
  if (solid(i32(g.x), i32(g.y), i32(g.z))) { return; }
  let own = pres[i];
  var sum: f32;
  // Clear of every boundary the stove can have: the six neighbours are gas.
  let ceiling = select(f32(gh()), P.stove.y, stove_on());
  if (g.x > 0u && g.y > bed_rows() && g.z > 0u && g.x + 1u < gw() && f32(g.y + 1u) < ceiling && g.y + 1u < gh() && g.z + 1u < gd()) {
    let sy = gw(); let sz = gw() * gh();
    sum = pres[i - 1u] + pres[i + 1u] + pres[i - sy] + pres[i + sy] + pres[i - sz] + pres[i + sz];
  } else {
    let p = vec3<i32>(g);
    sum = pres_at(p.x + 1, p.y, p.z, own) + pres_at(p.x - 1, p.y, p.z, own)
        + pres_at(p.x, p.y + 1, p.z, own) + pres_at(p.x, p.y - 1, p.z, own)
        + pres_at(p.x, p.y, p.z + 1, own) + pres_at(p.x, p.y, p.z - 1, own);
  }
  let gs = (sum - div[i]) / 6.0;
  pres[i] = own + SOR * (gs - own);
}

@compute @workgroup_size(4, 4, 4)
fn pres_red(@builtin(global_invocation_id) g: vec3<u32>) { relax(g, 0u); }

@compute @workgroup_size(4, 4, 4)
fn pres_black(@builtin(global_invocation_id) g: vec3<u32>) { relax(g, 1u); }

@compute @workgroup_size(4, 4, 4)
fn gradient(@builtin(global_invocation_id) g: vec3<u32>) {
  if (!inside(g)) { return; }
  let p = vec3<i32>(g);
  let i = idx(g.x, g.y, g.z);
  let own = pres[i];
  let grad = 0.5 * vec3<f32>(
    pres_at(p.x + 1, p.y, p.z, own) - pres_at(p.x - 1, p.y, p.z, own),
    pres_at(p.x, p.y + 1, p.z, own) - pres_at(p.x, p.y - 1, p.z, own),
    pres_at(p.x, p.y, p.z + 1, own) - pres_at(p.x, p.y, p.z - 1, own));
  if (solid(p.x, p.y, p.z)) {
    vel[i] = vec4<f32>(0.0);
    return;
  }
  vel[i] = vec4<f32>(tame(vel[i].xyz - grad), 0.0);
}

// ---------------------------------------------------------------- embers
//
// Four vec4s a spark: (position, heat), (velocity, cooling rate),
// (position one step ago, age in seconds), (bounces, off a wall, off the
// ceiling, 1 once it has gone up the flue). Heat at or below EMBER_OUT is an empty slot;
// Rust fills empty slots when the dice say a spark flies.

@compute @workgroup_size(16)
fn embers(@builtin(global_invocation_id) g: vec3<u32>) {
  let k = g.x;
  if (k >= P.coarse.w) { return; }
  var a = parts[4u * k];
  var b = parts[4u * k + 1u];
  var hits = parts[4u * k + 3u];
  if (a.w <= EMBER_OUT) { return; }
  let dt = P.a.x;
  let fluid = tri_vel(tap(a.xyz)).xyz;
  let around = tri_scal(tap(a.xyz)).x;
  // Small and light: the air has it within half a second, and it keeps a
  // lift of its own only while it is hot. A jitter of its own on top.
  let seed = P.dims.w * 64u + k * 4u;
  let jitter = vec3<f32>(rand(seed) - 0.5, rand(seed + 1u) - 0.5, rand(seed + 2u) - 0.5) * vec3<f32>(60.0, 30.0, 30.0) * P.stove.w;
  let lift = (45.0 * a.w * a.w + 6.0) * P.stove.w;
  // A burst's sparks (cooling slot past 1, card 376) are heavier chunks:
  // the air grips them less.
  let burst = b.w >= 1.0;
  let cooling = b.w - select(0.0, 1.0, burst);
  let grip = select(2.2, 0.7, burst);
  var v = b.xyz + dt * (grip * (fluid - b.xyz) + vec3<f32>(0.0, lift, 0.0) + jitter);
  let prev = a.xyz;
  var pos = a.xyz + dt * v;
  var heat = a.w - dt * cooling * a.w * (1.0 - 0.25 * around);
  // No spark lingers: one that has been alight eight seconds goes out.
  let age = parts[4u * k + 2u].w + dt;
  if (age > 8.0) { heat = 0.0; }

  // The firebox (card 376): the frame's left and right edges are walls and
  // its top is the ceiling. A spark that meets one bounces - most of its
  // speed into the wall is lost, some of its slide along it kept - and
  // keeps cooling. Only a real impact counts as a bounce; a spark the air
  // holds against the ceiling slides along it.
  let impact = EMBER_IMPACT * P.stove.w;
  // Where the frame's edges are at this depth: the camera is level and
  // looks along +z, so the half-width grows with distance.
  let half = (pos.z - P.eye.z) * length(P.right.xyz);
  let inset = 0.3 * f32(gw()) / 64.0;
  let xl = P.eye.x - half + inset;
  let xr = P.eye.x + half - inset;
  if (pos.x < xl) {
    if (-v.x > impact) { hits.x += 1.0; hits.y += 1.0; }
    pos.x = xl + (xl - pos.x) * EMBER_REST;
    v = vec3<f32>(abs(v.x) * EMBER_REST, v.y * EMBER_SLIDE, v.z * EMBER_SLIDE);
  } else if (pos.x > xr) {
    if (v.x > impact) { hits.x += 1.0; hits.y += 1.0; }
    pos.x = xr - (pos.x - xr) * EMBER_REST;
    v = vec3<f32>(-abs(v.x) * EMBER_REST, v.y * EMBER_SLIDE, v.z * EMBER_SLIDE);
  }
  if (stove_on() && !in_flue(pos.x, pos.z)) {
    // The ceiling where the frame shows it: the frame's top edge at this
    // depth, a little inside, so the bounce is seen (and never above the
    // gas's ceiling).
    let dz = pos.z - P.eye.z;
    let frame_top = P.eye.y + dz * (P.fwd.y / P.fwd.z + length(P.up.xyz)) - 1.5 * f32(gw()) / 64.0;
    let top = min(P.stove.y - 0.5, frame_top);
    if (pos.y > top) {
      if (v.y > impact) { hits.x += 1.0; hits.z += 1.0; }
      pos.y = top - (pos.y - top) * EMBER_REST;
      // A spark that hits hard skitters off sideways along the ceiling: a
      // little scatter, seeded like the rest of its dice.
      let skitter = (rand(seed + 3u) - 0.5) * 2.0 * max(v.y, 0.0);
      v = vec3<f32>(v.x * EMBER_SLIDE + skitter, -abs(v.y) * EMBER_REST, v.z * EMBER_SLIDE);
    }
  }
  // Up the flue, or out of an open fire's top: gone.
  if (pos.y > f32(gh()) - 2.0) {
    heat = 0.0;
    if (stove_on()) { hits.w = 1.0; }
  }
  // Front glass and back wall, unseen: kept inside, quietly.
  if (pos.z < 0.5) { pos.z = 0.5; v.z = abs(v.z) * EMBER_REST; }
  if (pos.z > f32(gd()) - 1.5) { pos.z = f32(gd()) - 1.5; v.z = -abs(v.z) * EMBER_REST; }
  if (pos.y < 0.5) { pos.y = 0.5; v.y = abs(v.y) * EMBER_REST; }
  parts[4u * k] = vec4<f32>(pos, heat);
  parts[4u * k + 1u] = vec4<f32>(v, b.w);
  parts[4u * k + 2u] = vec4<f32>(prev, age);
  parts[4u * k + 3u] = hits;
}

// ---------------------------------------------------------------- the fire's light

fn cw() -> u32 { return P.coarse.x; }
fn ch() -> u32 { return P.coarse.y; }
fn cd() -> u32 { return P.coarse.z; }
fn cidx(x: u32, y: u32, z: u32) -> u32 { return (z * ch() + y) * cw() + x; }

@compute @workgroup_size(4, 4, 4)
fn light_gather(@builtin(global_invocation_id) g: vec3<u32>) {
  if (g.x >= cw() || g.y >= ch() || g.z >= cd()) { return; }
  var sum = 0.0;
  for (var dz = 0u; dz < LIGHT_CELL; dz++) {
    for (var dy = 0u; dy < LIGHT_CELL; dy++) {
      for (var dx = 0u; dx < LIGHT_CELL; dx++) {
        let e = emit(scal[idx(g.x * LIGHT_CELL + dx, g.y * LIGHT_CELL + dy, g.z * LIGHT_CELL + dz)].x);
        sum += dot(e, vec3<f32>(0.2126, 0.7152, 0.0722));
      }
    }
  }
  light[cidx(g.x, g.y, g.z)] = sum / f32(LIGHT_CELL * LIGHT_CELL * LIGHT_CELL);
}

fn blur(g: vec3<u32>, axis: u32, from_first: bool) {
  if (g.x >= cw() || g.y >= ch() || g.z >= cd()) { return; }
  let n = i32(select(select(cd(), ch(), axis == 1u), cw(), axis == 0u));
  let p = vec3<i32>(g);
  var sum = 0.0;
  var count = 0.0;
  for (var o = -LIGHT_RADIUS; o <= LIGHT_RADIUS; o++) {
    var q = p;
    q[axis] = q[axis] + o;
    if (q[axis] < 0 || q[axis] >= n) { continue; }
    let j = cidx(u32(q.x), u32(q.y), u32(q.z));
    sum += select(light2[j], light[j], from_first);
    count += 1.0;
  }
  let i = cidx(g.x, g.y, g.z);
  if (from_first) { light2[i] = sum / count; } else { light[i] = sum / count; }
}

@compute @workgroup_size(4, 4, 4)
fn light_x(@builtin(global_invocation_id) g: vec3<u32>) { blur(g, 0u, true); }
@compute @workgroup_size(4, 4, 4)
fn light_y(@builtin(global_invocation_id) g: vec3<u32>) { blur(g, 1u, false); }
@compute @workgroup_size(4, 4, 4)
fn light_z(@builtin(global_invocation_id) g: vec3<u32>) { blur(g, 2u, true); }

// The light at a fine-grid position, trilinear on the coarse grid (light2).
fn light_at(p: vec3<f32>) -> f32 {
  let c = p / f32(LIGHT_CELL) - 0.5;
  let hi = vec3<f32>(f32(cw() - 1u), f32(ch() - 1u), f32(cd() - 1u));
  let q = clamp(c, vec3<f32>(0.0), hi);
  let b = min(vec3<u32>(q), vec3<u32>(cw() - 2u, ch() - 2u, cd() - 2u));
  let f = q - vec3<f32>(b);
  let i = cidx(b.x, b.y, b.z);
  let sy = cw(); let sz = cw() * ch();
  let x00 = mix(light2[i], light2[i + 1u], f.x);
  let x10 = mix(light2[i + sy], light2[i + sy + 1u], f.x);
  let x01 = mix(light2[i + sz], light2[i + sz + 1u], f.x);
  let x11 = mix(light2[i + sz + sy], light2[i + sz + sy + 1u], f.x);
  return mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z);
}

// ---------------------------------------------------------------- the picture

// Out: (fire light rgb, lit smoke) per LED, averaged over ss x ss rays, and
// at out[2048 + led] the same fire's light as if there were no smoke (its
// luminance, in x): the difference is how much soot is standing in front of it.
@compute @workgroup_size(8, 8, 1)
fn march(@builtin(global_invocation_id) g: vec3<u32>) {
  if (g.x >= 64u || g.y >= 32u) { return; }
  let ss = u32(P.eye.w);
  let lo = vec3<f32>(0.0);
  let hi = vec3<f32>(f32(gw() - 1u), f32(gh() - 1u), f32(gd() - 1u));
  let step = P.fwd.w;
  var acc = vec4<f32>(0.0);
  var bare_acc = 0.0;
  for (var sy = 0u; sy < ss; sy++) {
    for (var sx = 0u; sx < ss; sx++) {
      let u = (f32(g.x) + (f32(sx) + 0.5) / f32(ss)) / 32.0 - 1.0;
      let v = 1.0 - (f32(g.y) + (f32(sy) + 0.5) / f32(ss)) / 16.0;
      let dir = normalize(P.fwd.xyz + u * P.right.xyz + v * P.up.xyz);
      // Slab test against the volume.
      let inv = 1.0 / dir;
      let t0 = (lo - P.eye.xyz) * inv;
      let t1 = (hi - P.eye.xyz) * inv;
      let tn = max(max(min(t0.x, t1.x), min(t0.y, t1.y)), min(t0.z, t1.z));
      let tf = min(min(max(t0.x, t1.x), max(t0.y, t1.y)), max(t0.z, t1.z));
      if (tf <= max(tn, 0.0)) { continue; }
      // A fixed, per-sample offset into the first step: the supersamples
      // between them cover the step, and nothing changes frame to frame.
      var t = max(tn, 0.0) + step * (f32((sx * 7u + sy * 3u) % (ss * ss)) + 0.5) / f32(ss * ss);
      var trans = 1.0;
      var fire = vec3<f32>(0.0);
      var lit = 0.0;
      var bare = 0.0;
      loop {
        if (t > tf) { break; }
        let p = P.eye.xyz + t * dir;
        let s = tri_scal(tap(p));
        let e = emit(s.x) * (P.d.w * step);
        fire += trans * e;
        bare += dot(e, vec3<f32>(0.2126, 0.7152, 0.0722));
        let smoke = s.z;
        if (smoke > 0.002) {
          lit += trans * smoke * light_at(p) * (P.d.z * step);
          trans *= exp(-P.d.y * smoke * step);
        }
        t += step;
      }
      acc += vec4<f32>(fire, lit);
      bare_acc += bare;
    }
  }
  out[g.y * 64u + g.x] = acc / f32(ss * ss);
  out[2048u + g.y * 64u + g.x] = vec4<f32>(bare_acc / f32(ss * ss), 0.0, 0.0, 0.0);
}
