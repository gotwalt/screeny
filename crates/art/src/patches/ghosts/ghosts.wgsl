// A lit cloth mesh: one hue "ray" (per the project rule that `color = 0` is
// pure grayscale by construction, not by a separate check), shaded by a cool
// key light and a dim ambient, with a soft backlit glow standing in for
// subsurface transmission through thin fabric, and a curvature-based AO term
// for the folds' own shadow.
//
// Card 338: the face (eyes, the bridge between them, the mouth) used to be
// cut here, as a UV-space hole in this same curved, supersampled,
// motion-blurred surface - and card 336's own log named the limit that made:
// "a UV ellipse cut into a curved, supersampled, motion-blurred surface
// cannot make solid dark blocks at 64x32". The face is now stamped straight
// onto the panel grid in `mod.rs`, after this shader's own render (and its
// motion blur and bloom) are done, so this shader only ever draws lit cloth.

struct Scene {
  view_proj: mat4x4<f32>,
  light: vec4<f32>,   // key light direction (world), unused w
  params: vec4<f32>,  // hue, chroma, ao strength, unused
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
  let ao_strength = s.params.z;

  // Card 326: "soft shadows in the folds" - a valley (negative fold) darkens,
  // a bulge is left alone.
  let ao = clamp(1.0 + ao_strength * min(v.fold, 0.0), 0.35, 1.0);

  let l = (ambient + key * key_strength) * ao + back * back_strength + rim * rim_strength;

  let hue = s.params.x;
  let chroma = s.params.y;
  let body_alpha = s.alpha[v.slot];
  return vec4<f32>(oklch(clamp(l, 0.0, 1.0), chroma, hue), body_alpha);
}
