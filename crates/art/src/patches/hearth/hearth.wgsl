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
  wind: array<vec4<f32>, 40>,  // per row, four rows to a vec4
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
@group(0) @binding(9) var<storage, read> bed: array<vec2<f32>>;
@group(0) @binding(10) var<storage, read_write> light: array<f32>;
@group(0) @binding(11) var<storage, read_write> light2: array<f32>;
@group(0) @binding(12) var<storage, read_write> parts: array<vec4<f32>>;
@group(0) @binding(13) var<storage, read_write> out: array<vec4<f32>>;
@group(0) @binding(14) var<uniform> lut: array<vec4<f32>, 512>;

const BED_ROWS: u32 = 5u;
const SOR: f32 = 1.8;
const EMBER_OUT: f32 = 0.30;
const LIGHT_CELL: u32 = 4u;
const LIGHT_RADIUS: i32 = 6;

fn gw() -> u32 { return P.dims.x; }
fn gh() -> u32 { return P.dims.y; }
fn gd() -> u32 { return P.dims.z; }
fn idx(x: u32, y: u32, z: u32) -> u32 { return (z * gh() + y) * gw() + x; }
fn inside(g: vec3<u32>) -> bool { return g.x < gw() && g.y < gh() && g.z < gd(); }
fn wind_at(y: u32) -> f32 { return P.wind[y / 4u][y % 4u]; }

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
  let mid = p - 0.5 * dt * vel[i].xyz;
  return p - dt * tri_vel(tap(mid)).xyz;
}

// And where the backward step from p reads: the same trace, the other way.
fn ahead_of(p: vec3<f32>, i: u32) -> vec3<f32> {
  let dt = P.a.x;
  let mid = p + 0.5 * dt * vel[i].xyz;
  return p + dt * tri_vel(tap(mid)).xyz;
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
  var s = scal[i];
  var temp = s.x;
  var fuel = s.y;
  if (g.y < BED_ROWS) {
    // Most in the middle of the log's height, a little at its edges.
    let k = 1.0 - abs(f32(g.y) - 2.0) / 3.0;
    let b = bed[g.z * gw() + g.x];
    fuel = max(fuel, b.x * k);
    temp = max(temp, b.y * k);
  }
  let burnt = fuel * (1.0 - exp(-P.b.z * dt));
  let left = fuel - burnt;
  let cooled = temp - dt * (P.c.x * temp * temp * temp * temp + P.c.y * temp);
  temp = clamp(max(cooled, min(1.0, fuel * P.b.w)), 0.0, 1.0);
  let smoke = min(s.z * exp(-P.d.x * dt) + burnt * P.c.w, 2.0);
  scal[i] = vec4<f32>(temp, left, smoke, P.c.z * burnt / dt);

  var v = vel[i].xyz;
  v.y += dt * (P.a.y * temp - P.a.z * smoke);
  v.x += (wind_at(g.y) - v.x) * (1.0 - exp(-P.b.x * dt));
  v *= exp(-P.a.w * dt);
  if (g.y == 0u) { v.y = max(v.y, 0.0); }
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
  let here = vel_c(p.x, p.y, p.z);
  let below = select(vel_c(p.x, p.y - 1, p.z).y, 0.0, g.y == 0u);
  let d = 0.5 * (vel_c(p.x + 1, p.y, p.z).x - vel_c(p.x - 1, p.y, p.z).x
               + vel_c(p.x, p.y + 1, p.z).y - below
               + vel_c(p.x, p.y, p.z + 1).z - vel_c(p.x, p.y, p.z - 1).z);
  let i = idx(g.x, g.y, g.z);
  div[i] = d - scal[i].w;
}

// Pressure beyond the open faces is 0; across the floor it mirrors.
fn pres_at(x: i32, y: i32, z: i32, own: f32) -> f32 {
  if (y < 0) { return own; }
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
  let own = pres[i];
  var sum: f32;
  if (g.x > 0u && g.y > 0u && g.z > 0u && g.x + 1u < gw() && g.y + 1u < gh() && g.z + 1u < gd()) {
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
  vel[i] = vec4<f32>(vel[i].xyz - grad, 0.0);
}

// ---------------------------------------------------------------- embers
//
// Three vec4s a spark: (position, heat), (velocity, cooling rate),
// (position one step ago, -). Heat at or below EMBER_OUT is an empty slot;
// Rust fills empty slots when the dice say a spark flies.

@compute @workgroup_size(16)
fn embers(@builtin(global_invocation_id) g: vec3<u32>) {
  let k = g.x;
  if (k >= P.coarse.w) { return; }
  var a = parts[3u * k];
  var b = parts[3u * k + 1u];
  if (a.w <= EMBER_OUT) { return; }
  let dt = P.a.x;
  let fluid = tri_vel(tap(a.xyz)).xyz;
  let around = tri_scal(tap(a.xyz)).x;
  // Small and light: the air has it within half a second, and it keeps a
  // lift of its own only while it is hot. A jitter of its own on top.
  let seed = P.dims.w * 64u + k * 4u;
  let jitter = vec3<f32>(rand(seed) - 0.5, rand(seed + 1u) - 0.5, rand(seed + 2u) - 0.5) * vec3<f32>(60.0, 30.0, 30.0);
  let lift = 45.0 * a.w * a.w + 6.0;
  var v = b.xyz + dt * (2.2 * (fluid - b.xyz) + vec3<f32>(0.0, lift, 0.0) + jitter);
  let prev = a.xyz;
  let pos = a.xyz + dt * v;
  var heat = a.w - dt * b.w * a.w * (1.0 - 0.6 * around);
  let w = f32(gw());
  if (pos.y > f32(gh()) - 2.0 || pos.x < -6.0 || pos.x > w + 6.0 || pos.z < 0.0 || pos.z > f32(gd())) { heat = 0.0; }
  parts[3u * k] = vec4<f32>(pos, heat);
  parts[3u * k + 1u] = vec4<f32>(v, b.w);
  parts[3u * k + 2u] = vec4<f32>(prev, 0.0);
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

// Out: (fire light rgb, lit smoke) per LED, averaged over ss x ss rays.
@compute @workgroup_size(8, 8, 1)
fn march(@builtin(global_invocation_id) g: vec3<u32>) {
  if (g.x >= 64u || g.y >= 32u) { return; }
  let ss = u32(P.eye.w);
  let lo = vec3<f32>(0.0);
  let hi = vec3<f32>(f32(gw() - 1u), f32(gh() - 1u), f32(gd() - 1u));
  let step = P.fwd.w;
  var acc = vec4<f32>(0.0);
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
      loop {
        if (t > tf || trans < 0.01) { break; }
        let p = P.eye.xyz + t * dir;
        let s = tri_scal(tap(p));
        fire += trans * emit(s.x) * (P.d.w * step);
        let smoke = s.z;
        if (smoke > 0.002) {
          lit += trans * smoke * light_at(p) * (P.d.z * step);
          trans *= exp(-P.d.y * smoke * step);
        }
        t += step;
      }
      acc += vec4<f32>(fire, lit);
    }
  }
  out[g.y * 64u + g.x] = acc / f32(ss * ss);
}
