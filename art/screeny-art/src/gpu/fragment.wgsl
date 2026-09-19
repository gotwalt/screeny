// Harness for full-frame shader pieces. The piece supplies:
//
//   fn piece(uv: vec2<f32>) -> vec3<f32>   // linear-light colour
//
// uv is centred and aspect-correct with y up: y in -1..1, x in -2..2.

struct Uniforms {
  resolution: vec2<f32>,
  t: f32,     // seconds; drive all motion from this
  dt: f32,
  seed: f32,  // 0..1, fixed for the life of the piece
  params: array<vec4<f32>, 4>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;

// The piece's i-th parameter, in the order its ParamSpecs are listed.
fn P(i: u32) -> f32 {
  return u.params[i / 4u][i % 4u];
}

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
  return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  let uv = (pos.xy * 2.0 - u.resolution) / u.resolution.y * vec2<f32>(1.0, -1.0);
  return vec4<f32>(max(piece(uv), vec3<f32>(0.0)), 1.0);
}
