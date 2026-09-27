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
  // Card 322: a curvature/crease proxy from the leaf's own soft-body shell
  // (0 for the rigid motion-blur echoes, which still draw the flat rest
  // mesh - see `gpu.rs::push_leaf`'s doc). Negative at a real fold/valley
  // (the midrib crease, or wherever a gust or a landing bent the blade),
  // left alone at a bulge.
  @location(7) fold: f32,
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
  @location(7) fold: f32,
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
  out.fold = fold;
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
  // Fresnel-ish rim (card 322: "rim glint"): brightens the silhouette's own
  // edge - where the surface turns away from the camera, as at a real cupped
  // or curled leaf's own boundary - without a second geometry pass.
  // Genuinely view-dependent (unlike `ghosts`' fixed-`(0,0,1)` shortcut):
  // this camera can see a leaf from any angle as it tumbles.
  let rim = pow(1.0 - clamp(abs(facing), 0.0, 1.0), 3.0);
  // Card 322: "curl and crease catching the key light" - a real fold in the
  // shell (a landing's crease, a gust's momentary bend) darkens where it
  // valleys and is left alone where it bulges, exactly `ghosts::cloth`'s own
  // curvature-based AO idea, so a flex the physics actually made is a flex
  // the shading actually shows.
  let ao = clamp(1.0 + 0.55 * min(v.fold, 0.0), 0.55, 1.0);

  // A face square-on to the key light reaches `intensity = 1.0` exactly (so
  // `top_l`/`bot_l` are directly "how light this hue reads when well lit"),
  // a backlit translucent face reaches a dimmer glow, and a face that gets
  // neither sits at the ambient floor rather than vanishing to nothing.
  let ambient = 0.24;
  let intensity = (ambient + (1.0 - ambient) * diffuse) * ao + 0.70 * transmit;
  let base_l = mix(v.bot_l, v.top_l, top_amount);
  let l = clamp(base_l * intensity + rim * 0.10 * (ambient + diffuse), 0.0, 0.92);
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
