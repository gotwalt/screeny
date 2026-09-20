// Overland: an endless procedural world, painted entirely by palette index.
//
// Nothing here computes a colour. Every surface picks an entry (or a mix of two
// neighbouring entries, which the mapper turns into ordered dither) from the 32
// colours that overland.rs builds for the current time of day. Depth is shown
// with bands, occlusion and shadow, never with fog.
//
// Palette layout. Keep in step with overland.rs.
const SKY: u32 = 1u;     // 4: zenith .. horizon
const SUN: u32 = 5u;     // sun or moon disc; also water glints
const STAR: u32 = 6u;
const BEACON: u32 = 7u;  // pulses, by palette animation alone
const WATER: u32 = 8u;   // 2: near, far   (+1: unused spare kept for glints)
const LAND: u32 = 11u;   // 16: depth band * 4 + light step
const CAP: u32 = 27u;    // 3: lit, shaded, far
const TOWER: u32 = 30u;  // 2: lit, shaded

// P: 0 speed, 1 relief, 2 terraces, 3 sea level, 4 towers, 5 clearance
// X: 0..2 direction to the sun or moon, 3 night (0/1)

const FAR: f32 = 46.0;
const TOWER_CELL: f32 = 7.0;

fn hash21(p: vec2<f32>) -> f32 {
  var q = fract(vec3<f32>(p.xyx) * 0.1031);
  q += dot(q, q.yzx + 33.33);
  return fract((q.x + q.y) * q.z);
}

fn vnoise(p: vec2<f32>) -> f32 {
  let i = floor(p);
  let f = fract(p);
  let w = f * f * (3.0 - 2.0 * f);
  return mix(
    mix(hash21(i), hash21(i + vec2<f32>(1.0, 0.0)), w.x),
    mix(hash21(i + vec2<f32>(0.0, 1.0)), hash21(i + vec2<f32>(1.0, 1.0)), w.x),
    w.y);
}

// 0..1 before scaling. A broad octave lays out ranges and basins, so there are
// mountains on the horizon to layer; three finer octaves shape them. Anything
// finer is below one LED and would just alias. Terraces turn slopes into big
// flat shapes with hard risers, which is what survives at this size.
fn height01(p: vec2<f32>) -> f32 {
  let o = vec2<f32>(u.seed * 91.7, u.seed * 37.3);
  var q = p / 8.0 + o;
  var h = 0.0;
  var a = 0.5;
  for (var i = 0; i < 3; i++) {
    h += a * vnoise(q);
    q = mat2x2<f32>(1.6, 1.2, -1.2, 1.6) * q;
    a *= 0.5;
  }
  h = mix(h / 0.875, vnoise(p / 30.0 + o.yx), 0.55);
  h = smoothstep(0.2, 0.8, h);
  let n = 6.0;
  let stepped = (floor(h * n) + smoothstep(0.32, 0.68, fract(h * n))) / n;
  return mix(h, stepped, P(2));
}

fn height(p: vec2<f32>) -> f32 {
  return height01(p) * P(1);
}

// Monolith towers, at most one per grid cell and always well inside it, so the
// distance to the cell wall is a safe step when the cell is empty.
struct Tower { d: f32, top: f32 };

fn tower(p: vec3<f32>) -> Tower {
  let c = floor(p.xz / TOWER_CELL);
  let l = p.xz - (c + 0.5) * TOWER_CELL;
  let wall = 0.5 * TOWER_CELL - max(abs(l.x), abs(l.y)) + 1.2;
  let k = c + u.seed * 53.0;
  if (hash21(k) > P(4) * 0.7) { return Tower(wall, 0.0); }
  let jitter = (vec2<f32>(hash21(k + 3.1), hash21(k + 7.7)) - 0.5) * 3.0;
  let top = P(1) * (0.85 + 0.5 * hash21(k + 11.3)) + 1.2;
  let e = abs(l - jitter) - vec2<f32>(0.6);
  return Tower(min(max(max(e.x, e.y), p.y - top), wall), top);
}

fn sky(rd: vec3<f32>, sun: vec3<f32>) -> vec3<f32> {
  let away = acos(clamp(dot(rd, sun), -1.0, 1.0));
  if (away < 0.17) { return PAL(SUN); }
  // Bands run zenith -> horizon; the sky brightens by a band or so round the sun.
  var f = 3.0 * (1.0 - pow(clamp(rd.y / 0.5, 0.0, 1.0), 0.6));
  f += 1.3 * (1.0 - smoothstep(0.17, 0.6, away));
  if (X(3) > 0.5) {
    let g = vec2<f32>(atan2(rd.x, rd.z), rd.y) * 9.0;
    let cell = floor(g);
    if (hash21(cell + 0.5) > 0.9) {
      let at = vec2<f32>(hash21(cell + 1.7), hash21(cell + 9.2)) * 0.6 + 0.2;
      if (length(fract(g) - at) < 0.2) { return PAL(STAR); }
    }
  }
  return ramp(SKY, 4u, f, 0.3);
}

fn shade(uv: vec2<f32>) -> vec3<f32> {
  let sun = normalize(vec3<f32>(X(0), X(1), X(2)));
  let sea = P(3) * P(1);

  // Camera: forward along +z, weaving slowly, riding a smooth maximum of the
  // ground ahead so it climbs before a cliff instead of at it.
  let z = u.t * P(0);
  let xz = vec2<f32>(5.0 * sin(z * 0.021 + u.seed * TAU), z);
  var lift = 0.0;
  for (var i = 0; i < 5; i++) {
    lift += exp(2.0 * max(height(xz + vec2<f32>(0.0, f32(i) * 2.5)), sea));
  }
  let ro = vec3<f32>(xz.x, log(lift / 5.0) / 2.0 + P(5), xz.y);
  var rd = normalize(vec3<f32>(uv.x, uv.y - 0.2, 1.5));
  let yaw = rot2(0.28 * sin(u.t * 0.07) + 0.1 * cos(z * 0.021 + u.seed * TAU));
  let fwd = yaw * rd.xz;
  rd = vec3<f32>(fwd.x, rd.y, fwd.y);

  let ceiling = P(1) * 1.35 + 1.3;
  var t = 0.15;
  var hit = 0;  // 1 land, 2 tower
  var tw = Tower(0.0, 0.0);
  for (var i = 0; i < 96; i++) {
    let p = ro + rd * t;
    if (rd.y > 0.0 && p.y > ceiling) { break; }
    let dl = p.y - height(p.xz);
    tw = tower(p);
    if (dl < 0.004 * t) { hit = 1; break; }
    if (tw.d < 0.004 * t) { hit = 2; break; }
    t += max(min(dl * 0.4, tw.d), 0.012 * t);
    if (t > FAR) { break; }
  }

  // Water is a plane, so it needs no marching and runs to the horizon.
  if (rd.y < 0.0) {
    let t_sea = (sea - ro.y) / rd.y;
    if (hit == 0 || t_sea < t) {
      let pw = ro + rd * t_sea;
      // Glints are horizontal dashes, dense only in a narrow lane under the
      // light: the classic low-res way to say "water".
      let lane = smoothstep(0.965, 0.999, dot(normalize(rd.xz), normalize(sun.xz)));
      let spark = vnoise(vec2<f32>(pw.x * 0.3, pw.z * 3.0 - u.t * 0.5));
      if (spark > 0.9 - 0.42 * lane) { return PAL(SUN); }
      return ramp(WATER, 2u, t_sea / FAR, 0.5);
    }
  }
  if (hit == 0) { return sky(rd, sun); }

  let p = ro + rd * t;
  let band = 4.0 * pow(t / FAR, 0.7) - 0.5;

  if (hit == 2) {
    if (p.y > tw.top - 0.55) { return PAL(BEACON); }
    let c = floor(p.xz / TOWER_CELL);
    let k = c + u.seed * 53.0;
    let centre = (c + 0.5) * TOWER_CELL + (vec2<f32>(hash21(k + 3.1), hash21(k + 7.7)) - 0.5) * 3.0;
    let q = p.xz - centre;
    var n = vec3<f32>(sign(q.x), 0.0, 0.0);
    if (abs(q.y) > abs(q.x)) { n = vec3<f32>(0.0, 0.0, sign(q.y)); }
    return PAL(TOWER + select(1u, 0u, dot(n, sun) > 0.15));
  }

  // Normals from a wide stencil that grows with distance: far terrain shades
  // as a few broad planes instead of per-pixel noise.
  let e = vec2<f32>(0.04 + 0.02 * t, 0.0);
  let n = normalize(vec3<f32>(
    height(p.xz - e.xy) - height(p.xz + e.xy),
    2.0 * e.x,
    height(p.xz - e.yx) - height(p.xz + e.yx)));
  let ndl = dot(n, sun);

  // Cast shadows: a second, shorter march toward the light. At a low sun these
  // are the biggest shapes in the frame.
  var lit = 1.0;
  if (ndl > 0.0 && t < FAR * 0.7) {
    var ts = 0.35;
    for (var i = 0; i < 22; i++) {
      let ps = p + sun * ts;
      if (ps.y > ceiling) { break; }
      let dh = ps.y - height(ps.xz);
      if (dh < 0.0) { lit = 0.0; break; }
      ts += max(dh * 0.6, 0.3);
    }
  }
  // Slopes take their step from the light. Flats take it from their elevation,
  // like the tints on a relief map: neighbouring terraces become different flat
  // colours, which reads at a size where a lit gradient would not.
  let level = clamp((p.y / P(1) - P(3)) / (0.79 - P(3)), 0.0, 1.0);
  let flat = smoothstep(0.86, 0.97, n.y);
  var shade = mix(0.7 + 2.3 * clamp(ndl, 0.0, 1.0), 1.0 + 2.0 * level, flat);
  if (lit < 0.5 || ndl <= 0.0) { shade = 0.0; }

  if (p.y > P(1) * 0.79) {
    if (band > 1.6) { return PAL(CAP + 2u); }
    return PAL(CAP + select(1u, 0u, shade > 1.4));
  }

  let b = u32(clamp(floor(band), 0.0, 2.0));
  let seam = smoothstep(0.78, 1.0, clamp(band, 0.0, 3.0) - f32(b));
  return mix(ramp(LAND + 4u * b, 4u, shade, 0.1), ramp(LAND + 4u * (b + 1u), 4u, shade, 0.1), seam);
}
