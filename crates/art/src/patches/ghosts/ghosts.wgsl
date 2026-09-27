// A lit cloth mesh: one hue "ray" (per the project rule that `color = 0` is
// pure grayscale by construction, not by a separate check), shaded by a cool
// key light and a dim ambient, with a soft backlit glow standing in for
// subsurface transmission through thin fabric, a curvature-based AO term for
// the folds' own shadow, and true-dark eye holes cut in UV space so a fold
// squashes them the way a real hole in a draped sheet would.

struct Scene {
  view_proj: mat4x4<f32>,
  // xy = left eye UV centre, zw = right eye UV centre, one per ghost slot.
  eyes: array<vec4<f32>, 4>,
  light: vec4<f32>,   // key light direction (world), unused w
  params: vec4<f32>,  // hue, chroma, eye radius (uv), ao strength
  params2: vec4<f32>, // ambient, key strength, translucency strength, rim strength
  alpha: vec4<f32>,   // per-slot body opacity
};
@group(0) @binding(0) var<uniform> s: Scene;

struct Varying {
  @builtin(position) clip: vec4<f32>,
  @location(0) world: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) fold: f32,
  @location(4) @interpolate(flat) slot: u32,
};

@vertex
fn vs_main(
  @location(0) pos: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) fold: f32,
  @location(4) slot: f32,
) -> Varying {
  var out: Varying;
  out.clip = s.view_proj * vec4<f32>(pos, 1.0);
  out.world = pos;
  out.normal = normal;
  out.uv = uv;
  out.fold = fold;
  out.slot = u32(slot);
  return out;
}

@fragment
fn fs_main(v: Varying) -> @location(0) vec4<f32> {
  let n = normalize(v.normal);
  let light = normalize(s.light.xyz);
  let ndotl = dot(n, light);
  let key = max(ndotl, 0.0);
  // A soft glow through the far side, standing in for real subsurface
  // scattering: thin fabric lets a little of the key light bleed through
  // where the surface faces away from it.
  let back = max(-ndotl, 0.0);
  // Fresnel-ish rim, view-independent enough at this distance to be cheap:
  // brightens the silhouette's own edge without a second geometry pass.
  let view = normalize(vec3<f32>(0.0, 0.0, 1.0));
  let rim = pow(1.0 - clamp(abs(dot(n, view)), 0.0, 1.0), 3.0);

  let ambient = s.params2.x;
  let key_strength = s.params2.y;
  let back_strength = s.params2.z;
  let rim_strength = s.params2.w;
  let ao_strength = s.params.w;

  // Card 326: "soft shadows in the folds" - a valley (negative fold) darkens,
  // a bulge is left alone.
  let ao = clamp(1.0 + ao_strength * min(v.fold, 0.0), 0.35, 1.0);

  var l = (ambient + key * key_strength) * ao + back * back_strength + rim * rim_strength;

  // Eyes: true dark holes in UV space, so a fold squashes their shape the
  // way it would a real hole cut in draped fabric, and so they occlude
  // fully regardless of how translucent the body is elsewhere.
  let eyes = s.eyes[v.slot];
  let er = s.params.z;
  let dl = distance(v.uv, eyes.xy);
  let dr = distance(v.uv, eyes.zw);
  // WGSL's `smoothstep(low, high, x)` is only defined for `low < high`, so
  // the edge itself is written ascending-with-distance and then inverted -
  // relying on the reversed-argument trick (fine in the CPU palette code's
  // own hand-rolled `smoothstep`, undefined here) was the bug that made the
  // eyes not read as darker than the rest of the body at all.
  let dark = 1.0 - smoothstep(er - 0.02, er + 0.02, min(dl, dr));
  // An absolute override, not a multiplicative dimming: a fold catching the
  // key light full-on multiplies down to a still-visible grey, not a hole.
  // True dark means true dark whatever the light on the body around it.
  l = mix(l, 0.015, dark);

  let hue = s.params.x;
  let chroma = s.params.y;
  // "Where two overlap, or it crosses something, you can tell" - true dark
  // eyes stay fully opaque (occluding whatever is behind exactly, per the
  // body-alpha rule) even where the sheet's own body is translucent.
  let body_alpha = s.alpha[v.slot];
  let a = clamp(body_alpha + dark * (1.0 - body_alpha), 0.0, 1.0);
  return vec4<f32>(oklch(clamp(l, 0.0, 1.0), chroma, hue), a);
}
