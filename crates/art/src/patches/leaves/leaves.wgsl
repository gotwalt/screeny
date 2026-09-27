// A lit, double-sided leaf surface. Every vertex already carries its own
// world position (rotated, scaled and translated on the CPU - see
// `gpu.rs::push_leaf`), so this shader only applies the camera and shades.

struct Scene {
  view_proj: mat4x4<f32>,
  eye: vec4<f32>,
  light: vec4<f32>,  // direction *to* the light, xyz; w unused
};
@group(0) @binding(0) var<uniform> s: Scene;

struct Varying {
  @builtin(position) clip: vec4<f32>,
  @location(0) world: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) hue: f32,
  @location(3) chroma: f32,
  @location(4) top_l: f32,
  @location(5) bot_l: f32,
  @location(6) alpha: f32,
};

@vertex
fn vs_main(
  @location(0) pos: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) hue: f32,
  @location(3) chroma: f32,
  @location(4) top_l: f32,
  @location(5) bot_l: f32,
  @location(6) alpha: f32,
) -> Varying {
  var out: Varying;
  out.clip = s.view_proj * vec4<f32>(pos, 1.0);
  out.world = pos;
  out.normal = normal;
  out.hue = hue;
  out.chroma = chroma;
  out.top_l = top_l;
  out.bot_l = bot_l;
  out.alpha = alpha;
  return out;
}

@fragment
fn fs_main(v: Varying) -> @location(0) vec4<f32> {
  let n = normalize(v.normal);
  let to_eye = normalize(s.eye.xyz - v.world);
  let light = normalize(s.light.xyz);

  // Double-sided: which face the camera is actually looking at decides
  // whether this is the top (adaxial, "fresh") or the underside ("aged",
  // paler and duller) - card 321's "the two faces differ". A soft
  // transition through edge-on rather than a hard seam.
  let facing = dot(n, to_eye);
  let top_amount = smoothstep(-0.15, 0.15, facing);
  let nf = select(-n, n, facing >= 0.0);

  let diffuse = max(dot(nf, light), 0.0);
  // Translucency: a real cue the card asks for - a thin leaf glows from the
  // side away from the light when the light is behind it relative to the
  // camera. Strongest exactly there, because the two dot products it is
  // built from oppose each other everywhere else.
  let transmit = max(dot(-nf, light), 0.0);
  let half_v = normalize(light + to_eye);
  let glint = pow(max(dot(nf, half_v), 0.0), 46.0);

  let ambient = 0.05;
  let intensity = ambient + 0.95 * diffuse + 0.80 * transmit;
  let base_l = mix(v.bot_l, v.top_l, top_amount);
  let l = clamp(base_l * intensity, 0.0, 0.92);
  // A backlit leaf reads a little warmer, the way real chlorophyll and
  // carotenoid pigments transmit differently than they reflect.
  let hue = v.hue + transmit * 9.0;
  var col = oklch(l, v.chroma * (0.35 + 0.65 * intensity), hue);
  // A small, waxy specular glint on the sunlit side only - riding on the
  // leaf's own colour, never the flat white the panel is not allowed to
  // show (brief: no full-white fills).
  col = col + oklch(min(base_l + 0.22, 0.95), v.chroma * 0.25, hue) * glint * 0.45 * diffuse;

  return vec4<f32>(col, v.alpha);
}
