// Flight through an endless lattice of solids, raymarched.
// P: 0 speed, 1 spacing, 2 size, 3 shape (0 sphere .. 1 box), 4 depth (cells),
//    5 hue, 6 hue spread, 7 samples (unused here)

fn cell_of(p: vec3<f32>) -> vec3<f32> {
  return round(p / P(1));
}

fn solid(p: vec3<f32>) -> f32 {
  let q = p - P(1) * cell_of(p);
  let r = P(2) * P(1) * 0.5;
  let ball = length(q) - r;
  let e = abs(q) - vec3<f32>(r * 0.72);
  let box = length(max(e, vec3<f32>(0.0))) + min(max(e.x, max(e.y, e.z)), 0.0) - r * 0.12;
  return mix(ball, box, P(3));
}

fn normal_at(p: vec3<f32>) -> vec3<f32> {
  let e = vec2<f32>(0.002 * P(1), 0.0);
  return normalize(vec3<f32>(
    solid(p + e.xyy) - solid(p - e.xyy),
    solid(p + e.yxy) - solid(p - e.yxy),
    solid(p + e.yyx) - solid(p - e.yyx),
  ));
}

fn piece(uv: vec2<f32>) -> vec3<f32> {
  let t = u.t * P(0);
  // Fly down the gap between rows (half a cell off the lattice), swaying a little.
  let sway = vec2<f32>(sin(t * 0.37), cos(t * 0.29)) * 0.12 * P(1);
  let origin = vec3<f32>(vec2<f32>(0.5 * P(1)) + sway, t * P(1));
  var dir = normalize(vec3<f32>(uv, 1.6));
  let roll = rot2(0.35 * sin(t * 0.21) + u.seed * TAU);
  let yaw = rot2(0.25 * sin(t * 0.17));
  dir = vec3<f32>(roll * dir.xy, dir.z);
  let xz = yaw * dir.xz;
  dir = vec3<f32>(xz.x, dir.y, xz.y);

  let far = P(4) * P(1);
  var d = 0.0;
  var hit = false;
  for (var i = 0; i < 72; i++) {
    let s = solid(origin + dir * d);
    if (s < 0.001 * d + 0.0005) { hit = true; break; }
    // Never step past the next cell boundary's worth of distance.
    d += min(s, P(1) * 0.5);
    if (d > far) { break; }
  }
  if (!hit) { return vec3<f32>(0.0); }

  let p = origin + dir * d;
  let n = normal_at(p);
  let id = cell_of(p);
  let hue = P(5) + P(6) * (hash13(id + u.seed * 17.0) - 0.5);
  let key = max(dot(n, normalize(vec3<f32>(-0.5, 0.7, -0.6))), 0.0);
  let rim = pow(1.0 - max(dot(n, -dir), 0.0), 3.0);
  // Lit faces stay in the panel's mid-to-bright range; shadow is a flat step
  // down, not a long dark gradient.
  var col = oklch(0.42 + 0.48 * key, 0.21, hue) + oklch(0.78, 0.16, hue + 150.0) * rim * 0.4;
  // Distant solids drop out over the last third of the range instead of
  // dimming all the way back.
  return col * (1.0 - smoothstep(far * 0.66, far, d));
}
