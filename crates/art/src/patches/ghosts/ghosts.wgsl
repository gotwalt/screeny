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
  params: vec4<f32>,  // hue, chroma, eye luminance (0 = true black), ao strength
  params2: vec4<f32>, // ambient, key strength, translucency strength, rim strength
  alpha: vec4<f32>,   // per-slot body opacity
  // eye radius across the seam (u), eye radius up/down (v), edge softness,
  // eye hue - separate u/v radii because a UV unit is not the same physical
  // size in both directions (u runs the whole way round the head, v only
  // top to hem), so a plain `distance()` in UV space drew a short, wide
  // ellipse when a round-ish hole was wanted.
  eye_shape: vec4<f32>,
  // x = each eye's own mirrored tilt, radians (card 336: "tilted (sad/
  // spooky)") - yzw unused.
  face: vec4<f32>,
  // xy = the mouth's own fixed UV centre, the same for every ghost slot
  // (unlike the eyes, never gaze-shifted) - zw unused.
  mouth: vec4<f32>,
  // mouth radius u, radius v, edge softness, `mouth` param (0 = none) -
  // the same u/v-anisotropy reasoning as `eye_shape`.
  mouth_shape: vec4<f32>,
};
@group(0) @binding(0) var<uniform> s: Scene;

// The normalised ellipse distance (1.0 at the shape's own edge) for a hole
// centred at `c` with UV radii `ru`/`rv`, tilted by `tilt` radians. The tilt
// is applied after bringing `v` into "u-equivalent" units (scaling by
// `ru/rv`, the same anisotropy correction the radii themselves already
// encode) so a rotation here reads as an actual tilt of the oval's own axes
// in LEDs, not a shear across two UV axes of different physical scale.
fn tilted_ellipse_dist(uv: vec2<f32>, c: vec2<f32>, ru: f32, rv: f32, tilt: f32) -> f32 {
  let d = uv - c;
  let scale = ru / rv;
  let iso = vec2<f32>(d.x, d.y * scale);
  let ct = cos(tilt);
  let st = sin(tilt);
  let rot = vec2<f32>(ct * iso.x - st * iso.y, st * iso.x + ct * iso.y);
  let back = vec2<f32>(rot.x, rot.y / scale);
  return length(vec2<f32>(back.x / ru, back.y / rv));
}

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

  // Eyes: a hole cut in UV space, so a fold squashes their shape the way it
  // would a real hole in draped fabric. `eye_light` (owner request) runs
  // them from true-dark holes (0) to a soft emissive glow (1) that blooms a
  // little into the cloth around them - never a plain multiply on the body's
  // own lighting either way, since a fold catching the key light full-on
  // must not wash a dark eye to a visible grey, and a bright fold must not
  // wash a glowing eye out either.
  let eyes = s.eyes[v.slot];
  let er_u = s.eye_shape.x;
  let er_v = s.eye_shape.y;
  let edge = s.eye_shape.z;
  let eye_hue = s.eye_shape.w;
  let eye_light = s.params.z;
  let tilt = s.face.x;
  // Tilted, normalised ellipse distance (1.0 at the eye's own edge) - each
  // eye mirrored, so they read as a matched, "sad/spooky" pair rather than
  // both leaning the same way (card 336).
  let dl = tilted_ellipse_dist(v.uv, eyes.xy, er_u, er_v, tilt);
  let dr = tilted_ellipse_dist(v.uv, eyes.zw, er_u, er_v, -tilt);
  let d = min(dl, dr);
  // WGSL's `smoothstep(low, high, x)` is only defined for `low < high`, so
  // the edge itself is written ascending-with-distance and then inverted -
  // relying on the reversed-argument trick (fine in the CPU palette code's
  // own hand-rolled `smoothstep`, undefined here) was the bug that once made
  // the eyes not read as darker than the rest of the body at all.
  let core = 1.0 - smoothstep(1.0 - edge, 1.0 + edge, d);
  // The glow's own bloom into the surrounding cloth: a soft falloff past the
  // core, out to a couple of eye-radii, contributing nothing where the core
  // itself already fully replaced the body's own lighting.
  const GLOW_REACH: f32 = 1.45;
  let spill = clamp(1.0 - d / GLOW_REACH, 0.0, 1.0) * (1.0 - core);

  // `eye_light == 0` is exactly the old absolute-override black hole;
  // `eye_light == 1` replaces the core with a bright, still-not-full-white
  // emissive level and adds the halo on top of the body's own lighting.
  let eye_core_l = mix(0.015, 0.92, eye_light);
  l = mix(l, eye_core_l, core);
  l = l + spill * eye_light * 0.55;

  let hue = s.params.x;
  let chroma = s.params.y;
  // The eye's own hue only matters once it is actually lit; blending it in
  // whether or not `eye_light > 0` would be free of visible effect at 0 (the
  // core sits at l = 0.015, indistinguishable by hue) but wrong in spirit,
  // so it is scaled by `eye_light` explicitly rather than relying on that.
  let frag_hue = mix(hue, eye_hue, clamp(core + spill, 0.0, 1.0) * eye_light);

  // Mouth: a frowning open mouth below the eyes (card 336), always a true
  // dark hole (no `eye_light`-style glow was asked for). Built as a crescent
  // - a wide "frown" outline with a smaller, higher inner cut carved out of
  // it - rather than a plain oval, so it reads as an open, downturned mouth
  // and not a second pair of eyes.
  let mr_u = s.mouth_shape.x;
  let mr_v = s.mouth_shape.y;
  let m_edge = s.mouth_shape.z;
  let m_amount = s.mouth_shape.w;
  let d_outer = tilted_ellipse_dist(v.uv, s.mouth.xy, mr_u, mr_v, 0.0);
  let inner_c = vec2<f32>(s.mouth.x, s.mouth.y - mr_v * 0.6);
  let d_inner = tilted_ellipse_dist(v.uv, inner_c, mr_u * 0.88, mr_v * 0.85, 0.0);
  let m_outer = 1.0 - smoothstep(1.0 - m_edge, 1.0 + m_edge, d_outer);
  let m_inner = 1.0 - smoothstep(1.0 - m_edge, 1.0 + m_edge, d_inner);
  let mouth_dark = clamp(m_outer - m_inner, 0.0, 1.0) * step(0.001, m_amount);
  l = mix(l, 0.015, mouth_dark);

  // "Where two overlap, or it crosses something, you can tell" - the dark
  // end of the eye (and the mouth, always dark) stays fully opaque
  // (occluding whatever is behind exactly, per the body-alpha rule) even
  // where the sheet's own body is translucent; a glowing eye is part of the
  // body's own surface, not an extra occluder.
  let body_alpha = s.alpha[v.slot];
  let dark_hole = clamp(core * (1.0 - eye_light) + mouth_dark, 0.0, 1.0);
  let a = clamp(body_alpha + dark_hole * (1.0 - body_alpha), 0.0, 1.0);
  return vec4<f32>(oklch(clamp(l, 0.0, 1.0), chroma, frag_hue), a);
}
