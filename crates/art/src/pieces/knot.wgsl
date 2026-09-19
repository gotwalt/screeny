// A lit tube mesh. Vertices carry the tube's centre line and outward normal;
// thickness is applied here so it can change without rebuilding the mesh.

struct Scene {
  view_proj: mat4x4<f32>,
  model: mat4x4<f32>,
  eye: vec4<f32>,
  look: vec4<f32>,  // tube radius, hue, hue spread, unused
};
@group(0) @binding(0) var<uniform> s: Scene;

struct Varying {
  @builtin(position) clip: vec4<f32>,
  @location(0) world: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) along: f32,
};

@vertex
fn vs_main(@location(0) centre: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) along: f32) -> Varying {
  let world = s.model * vec4<f32>(centre + normal * s.look.x, 1.0);
  var out: Varying;
  out.clip = s.view_proj * world;
  out.world = world.xyz;
  out.normal = (s.model * vec4<f32>(normal, 0.0)).xyz;
  out.along = along;
  return out;
}

@fragment
fn fs_main(v: Varying) -> @location(0) vec4<f32> {
  let n = normalize(v.normal);
  let to_eye = normalize(s.eye.xyz - v.world);
  // Hue runs out and back along the knot, so it meets itself at the seam.
  let hue = s.look.y + s.look.z * (0.5 - 0.5 * cos(v.along * TAU));
  let key = max(dot(n, normalize(vec3<f32>(-0.4, 0.8, 0.5))), 0.0);
  let rim = pow(1.0 - max(dot(n, to_eye), 0.0), 2.5);
  // Wide lightness range so the tube reads as round, but the floor stays well
  // above the panel's crushed darks; a saturated rim separates the crossings.
  let col = oklch(0.4 + 0.5 * key * key, 0.23, hue) + oklch(0.78, 0.17, hue + 160.0) * rim * 0.45;
  return vec4<f32>(col, 1.0);
}
