// The box and the hand, raymarched together: a real SDF with smooth unions
// for the palm pad, the knuckles and the finger taper (card 334), rather
// than card 333's supersampled capsule silhouette. `mod.rs` builds one
// `Prim` per bone/knuckle/nail every frame from the same forward-kinematics
// `hand_rig::Pose::solve` already produces (unchanged - this file only
// replaces the *render*).
//
// Two lessons paid for elsewhere in this codebase, both load-bearing here:
// - `smoothstep(low, high, x)` is undefined in WGSL when `low > high` (card
//   326's review). Every soft edge below (`smin`, `smooth01`, the nail
//   cutoff, the box's corner AO) is written as an explicit `clamp` and a
//   hand-rolled cubic, never the builtin, so there is no ordering to get
//   wrong.
// - a surface's *outward* normal has to be checked against something real
//   (also card 326): `normal_at` is checked here by
//   `crates/art/tests/... ` via `mod.rs`'s own render tests reading actual
//   `Frame::pixel` lightness, not assumed from the gradient formula alone.

struct Prim {
  // kind (0 = capsule, 1 = sphere), chain (0..3 fingers, 4 thumb, 5 palm),
  // material (0 = skin, 1 = nail), unused.
  info: vec4<f32>,
  a: vec4<f32>, // xyz + radius-at-a (capsule) or centre + radius (sphere)
  b: vec4<f32>, // xyz + radius-at-b (capsule); unused for a sphere
}

const MAX_PRIM: u32 = 48u;
// How far past a nail primitive's own surface its highlight still shows,
// world metres - the nail's material "bleeds" this far into the smoothly
// unioned skin around it.
const NAIL_EPS: f32 = 0.012;

struct Uniforms {
  resolution: vec2<f32>,
  prim_count: f32,
  _pad0: f32,
  eye: vec4<f32>,
  right: vec4<f32>,
  up: vec4<f32>,
  fwd: vec4<f32>,
  // focal length, hue (degrees), chroma, blend radius within one finger.
  focal_hue_chroma_kfinger: vec4<f32>,
  // bulb position (xyz), light attenuation coefficient.
  light_attk: vec4<f32>,
  // box half-width, depth, ceiling height, floor height.
  box_dims: vec4<f32>,
  // contact shadow: centre x, centre z, strength, radius^2.
  shadow: vec4<f32>,
  // ambient, key (point-light) strength, subsurface/translucency, rim.
  light_strength: vec4<f32>,
  // AO strength, nail highlight boost, unused, dark cutoff
  // (floor brightness below which it is held to true black).
  ao_nail_kpalm_darkcut: vec4<f32>,
  // floor's max lightness once lit, palm's own self-blend radius, unused x2.
  maxl_kpalmself: vec4<f32>,
  prims: array<Prim, MAX_PRIM>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
  return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

// A ray through pixel `pos` (device coordinates, before the y-flip), from
// `box_scene::Camera::ray`'s own formula.
fn cam_ray(pos: vec2<f32>) -> vec3<f32> {
  let px = (pos.x - 0.5 * u.resolution.x) / u.focal_hue_chroma_kfinger.x;
  let py = (0.5 * u.resolution.y - pos.y) / u.focal_hue_chroma_kfinger.x;
  return normalize(u.fwd.xyz + u.right.xyz * px + u.up.xyz * py);
}

fn sd_capsule(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, ra: f32, rb: f32) -> f32 {
  let pa = p - a;
  let ba = b - a;
  let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-8), 0.0, 1.0);
  return length(pa - ba * h) - mix(ra, rb, h);
}

fn sd_sphere(p: vec3<f32>, a: vec3<f32>, r: f32) -> f32 {
  return length(p - a) - r;
}

// Polynomial smooth minimum (iq): always defined for `k > 0`, unlike a
// `smoothstep`-based blend, and gives the "material" mix `h` a sharp union
// (`k` near zero) needs anyway, so it is used for every union here.
fn smin(a: f32, b: f32, k: f32) -> f32 {
  let kk = max(k, 1e-5);
  let h = clamp(0.5 + 0.5 * (b - a) / kk, 0.0, 1.0);
  return mix(b, a, h) - kk * h * (1.0 - h);
}

// The scene's signed distance: every primitive smooth-unioned with its own
// finger (or the palm/wrist), then every finger and the thumb smooth-joined
// onto the palm - never directly with each other, which is what keeps five
// separate fingers from ever reading as a mitten (see `mod.rs`'s doc for the
// full reasoning). Chain 5 is the palm/wrist; 0..3 are the four fingers; 4
// is the thumb.
fn scene_sdf(p: vec3<f32>) -> f32 {
  var acc: array<f32, 6>;
  for (var c = 0u; c < 6u; c++) {
    acc[c] = 1.0e5;
  }
  let k_finger = u.focal_hue_chroma_kfinger.w;
  let k_palm_self = u.maxl_kpalmself.y;
  let n = u32(u.prim_count);
  for (var i = 0u; i < MAX_PRIM; i++) {
    if (i >= n) { break; }
    let m = u.prims[i].info;
    let av = u.prims[i].a;
    let bv = u.prims[i].b;
    var d: f32;
    if (m.x < 0.5) {
      d = sd_capsule(p, av.xyz, bv.xyz, av.w, bv.w);
    } else {
      d = sd_sphere(p, av.xyz, av.w);
    }
    let chain = u32(m.y);
    let k = select(k_finger, k_palm_self, chain == 5u);
    acc[chain] = smin(acc[chain], d, k);
  }
  // Hard union between fingers and the palm, and between fingers
  // themselves - not `smin`. `smin(total, acc[c], k)` looks like it only
  // ever blends `acc[c]` into the palm, but `total` already carries every
  // finger folded in by the time a later finger is combined, so a smooth
  // blend here would let *any two fingers* bridge wherever they are
  // roughly equidistant from that shared accumulator (the gap between two
  // adjacent fingertips is exactly such a place) - a real bug found by
  // rendering, not by inspection: `wave`'s open, spread fingers still came
  // out a single blob until this changed to a hard `min`. Each finger's
  // own joints and the palm's own capsules stay smoothly unioned
  // (`k_finger`/`k_palm_self` above); only the seam between separate
  // structures is a plain, un-softened minimum, which is what keeps five
  // fingers five fingers.
  var total = acc[5];
  for (var c = 0u; c < 5u; c++) {
    total = min(total, acc[c]);
  }
  return total;
}

fn normal_at(p: vec3<f32>) -> vec3<f32> {
  let e = vec2<f32>(0.0006, 0.0);
  return normalize(vec3<f32>(
    scene_sdf(p + e.xyy) - scene_sdf(p - e.xyy),
    scene_sdf(p + e.yxy) - scene_sdf(p - e.yxy),
    scene_sdf(p + e.yyx) - scene_sdf(p - e.yyx),
  ));
}

// Cheap raymarched ambient occlusion (iq's five-tap version): how much the
// scene closes in around a point along its own normal - the "ambient
// occlusion between fingers" the card asks for, and the only thing that
// makes the web between two fingers read as a crease rather than a flat
// continuation of both.
fn ao_at(p: vec3<f32>, n: vec3<f32>) -> f32 {
  var occ = 0.0;
  var sca = 1.0;
  for (var i = 0; i < 5; i++) {
    let h = 0.006 + 0.032 * f32(i) / 4.0;
    let d = scene_sdf(p + n * h);
    occ += (h - d) * sca;
    sca *= 0.6;
  }
  return clamp(1.0 - u.ao_nail_kpalm_darkcut.x * occ, 0.0, 1.0);
}

// Manual smoothstep-shaped cubic, always ascending by construction
// (`x` is pre-clamped to `0..1` here) - the safe replacement for
// `smoothstep(a, b, x)` wherever `a <= b` cannot be guaranteed by
// inspection alone (card 326's own lesson).
fn smooth01(x: f32) -> f32 {
  let t = clamp(x, 0.0, 1.0);
  return t * t * (3.0 - 2.0 * t);
}

fn shade_hand(p: vec3<f32>, n: vec3<f32>, viewdir: vec3<f32>) -> vec3<f32> {
  let light_pos = u.light_attk.xyz;
  let to_light = light_pos - p;
  let dist2 = max(dot(to_light, to_light), 1e-8);
  let att = 1.0 / (1.0 + u.light_attk.w * dist2);
  let ldir = to_light * inverseSqrt(dist2);
  let lam = max(dot(n, ldir), 0.0);
  let ao = ao_at(p, n);

  let ambient = u.light_strength.x;
  let key = u.light_strength.y;
  let back = u.light_strength.z;
  let rim_s = u.light_strength.w;

  let base_l = (ambient + key * lam) * att * ao;
  // A hint of subsurface warmth: light wrapping through from the far side
  // (the classic "wrap" translucency hack) plus a view-dependent Fresnel
  // rim, both riding the thin, near-silhouette parts of the hand where a
  // real hand would glow faintly red-gold against a light behind it.
  let wrap = max(dot(-n, ldir), 0.0);
  let translucent = back * wrap * att;
  let rim = rim_s * pow(1.0 - max(dot(n, -viewdir), 0.0), 3.0);
  var l = clamp(base_l + translucent, 0.0, 1.0) + rim;

  // Nails: a soft highlight wherever the surface point sits inside (or just
  // outside, within a small margin) one of the nail primitives - the same
  // smooth union that shaped the geometry also gives the material blend
  // its own natural, soft edge here, entirely without touching the SDF's
  // own chain accumulation.
  var nail_mask = 0.0;
  let n_count = u32(u.prim_count);
  for (var i = 0u; i < MAX_PRIM; i++) {
    if (i >= n_count) { break; }
    let m = u.prims[i].info;
    if (m.z > 0.5) {
      let av = u.prims[i].a;
      let dn = length(p - av.xyz) - av.w;
      nail_mask = max(nail_mask, smooth01((NAIL_EPS - dn) / (2.0 * NAIL_EPS)));
    }
  }
  l = clamp(l + nail_mask * u.ao_nail_kpalm_darkcut.y, 0.0, 1.0);

  return oklch(l, u.focal_hue_chroma_kfinger.z, u.focal_hue_chroma_kfinger.y);
}

struct BoxHit {
  p: vec3<f32>,
  t: f32,
  plane: f32, // 0 floor, 1 ceiling, 2 left, 3 right, 4 back, -1 none
}

// The nearest of the box's five planes a ray reaches - `box_scene.rs`'s own
// `intersect`, ported (the CPU version is gone: this file is now the only
// place the box itself is drawn).
fn box_hit(eye: vec3<f32>, dir: vec3<f32>) -> BoxHit {
  let half_w = u.box_dims.x;
  let depth = u.box_dims.y;
  let ceil_y = u.box_dims.z;
  let floor_y = u.box_dims.w;
  var best_t = 1.0e30;
  var best_plane = -1.0;
  var best_p = vec3<f32>(0.0);
  if (abs(dir.y) > 1e-6) {
    let t0 = (floor_y - eye.y) / dir.y;
    let h0 = eye + dir * t0;
    if (t0 > 1e-4 && abs(h0.x) <= half_w && h0.z >= 0.0 && h0.z <= depth && t0 < best_t) {
      best_t = t0; best_plane = 0.0; best_p = h0;
    }
    let t1 = (ceil_y - eye.y) / dir.y;
    let h1 = eye + dir * t1;
    if (t1 > 1e-4 && abs(h1.x) <= half_w && h1.z >= 0.0 && h1.z <= depth && t1 < best_t) {
      best_t = t1; best_plane = 1.0; best_p = h1;
    }
  }
  if (abs(dir.x) > 1e-6) {
    let t0 = (-half_w - eye.x) / dir.x;
    let h0 = eye + dir * t0;
    if (t0 > 1e-4 && h0.y >= floor_y && h0.y <= ceil_y && h0.z >= 0.0 && h0.z <= depth && t0 < best_t) {
      best_t = t0; best_plane = 2.0; best_p = h0;
    }
    let t1 = (half_w - eye.x) / dir.x;
    let h1 = eye + dir * t1;
    if (t1 > 1e-4 && h1.y >= floor_y && h1.y <= ceil_y && h1.z >= 0.0 && h1.z <= depth && t1 < best_t) {
      best_t = t1; best_plane = 3.0; best_p = h1;
    }
  }
  if (dir.z > 1e-6) {
    let t0 = (depth - eye.z) / dir.z;
    let h0 = eye + dir * t0;
    if (t0 > 1e-4 && abs(h0.x) <= half_w && h0.y >= floor_y && h0.y <= ceil_y && t0 < best_t) {
      best_t = t0; best_plane = 4.0; best_p = h0;
    }
  }
  return BoxHit(best_p, best_t, best_plane);
}

// True black off the floor (the autumn set's "foreground animations against
// a black backdrop"); on the floor, a lambertian pool of light under the
// bulb, corner ambient occlusion, and the hand's own contact shadow.
fn shade_box(p: vec3<f32>, plane: f32) -> vec3<f32> {
  if (plane > 0.5) {
    return vec3<f32>(0.0);
  }
  let n = vec3<f32>(0.0, 1.0, 0.0);
  let light_pos = u.light_attk.xyz;
  let to_light = light_pos - p;
  let dist2 = max(dot(to_light, to_light), 1e-8);
  let att = 1.0 / (1.0 + u.light_attk.w * dist2);
  let lam = max(dot(n, to_light * inverseSqrt(dist2)), 0.0);

  let half_w = u.box_dims.x;
  let depth = u.box_dims.y;
  let nearest = min(min(p.x - (-half_w), half_w - p.x), depth - p.z);
  let corner_ao = 0.55 + 0.45 * smooth01(nearest / 0.085);

  var b = lam * att * corner_ao;
  let sx = u.shadow.x;
  let sz = u.shadow.y;
  let sstr = u.shadow.z;
  let srad2 = max(u.shadow.w, 1e-6);
  let d2 = (p.x - sx) * (p.x - sx) + (p.z - sz) * (p.z - sz);
  b *= 1.0 - sstr * exp(-d2 / srad2);

  let dark_cut = u.ao_nail_kpalm_darkcut.w;
  let max_l = u.maxl_kpalmself.x;
  let lit = smooth01((b - dark_cut) / max(1.0 - dark_cut, 1e-4));
  let l = max_l * pow(max(lit, 0.0), 1.0 / 3.0);
  return oklch(l, u.focal_hue_chroma_kfinger.z, u.focal_hue_chroma_kfinger.y);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  let eye = u.eye.xyz;
  let dir = cam_ray(pos.xy);
  let bh = box_hit(eye, dir);
  var max_t = 1.0e6;
  if (bh.plane >= 0.0) {
    max_t = bh.t;
  }

  var t = 0.0;
  var hit = false;
  for (var i = 0; i < 80; i++) {
    let p = eye + dir * t;
    let d = scene_sdf(p);
    if (d < 0.0005) {
      hit = true;
      break;
    }
    t += d;
    if (t > max_t) {
      break;
    }
  }

  var col = vec3<f32>(0.0);
  if (hit) {
    let p = eye + dir * t;
    let n = normal_at(p);
    col = shade_hand(p, n, dir);
  } else if (bh.plane >= 0.0) {
    col = shade_box(bh.p, bh.plane);
  }
  return vec4<f32>(max(col, vec3<f32>(0.0)), 1.0);
}
