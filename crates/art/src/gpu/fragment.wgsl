// Harness for full-frame shader patches. The patch supplies:
//
//   fn shade(uv: vec2<f32>) -> vec3<f32>   // linear-light colour
//
// and gets u.t / u.seed, its parameters as P(i), and, if it has a scene
// function, per-frame values as X(i) and a palette as PAL(i) / ramp().
//
// uv is centred and aspect-correct with y up: y in -1..1, x in -2..2.

struct Uniforms {
  resolution: vec2<f32>,
  t: f32,     // seconds; drive all motion from this
  dt: f32,
  seed: f32,  // 0..1, fixed for the life of the patch
  params: array<vec4<f32>, 4>,
  extra: array<vec4<f32>, 4>,
  palette: array<vec4<f32>, 32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;

// The patch's i-th parameter, in the order its ParamSpecs are listed.
fn P(i: u32) -> f32 {
  return u.params[i / 4u][i % 4u];
}

// The i-th value the patch's scene function computed for this frame.
fn X(i: u32) -> f32 {
  return u.extra[i / 4u][i % 4u];
}

// Entry i of the patch's palette (linear light). Painting only with these, and
// with mixes of two of them, keeps a frame inside the panel's colour budget:
// the mapper turns pure entries into themselves and mixes into ordered dither.
fn PAL(i: u32) -> vec3<f32> {
  return u.palette[i].rgb;
}

// A ramp of n palette entries starting at base, sampled at x in 0..n-1.
// soft = 0.5 dithers the whole way between neighbours (a gradient);
// soft near 0 gives hard bands with a one-pixel dithered seam.
fn ramp(base: u32, n: u32, x: f32, soft: f32) -> vec3<f32> {
  let xc = clamp(x, 0.0, f32(n - 1u));
  let i = min(u32(floor(xc)), n - 2u);
  let k = smoothstep(0.5 - soft, 0.5 + soft, xc - f32(i));
  return mix(PAL(base + i), PAL(base + i + 1u), k);
}

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
  let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
  return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
  let uv = (pos.xy * 2.0 - u.resolution) / u.resolution.y * vec2<f32>(1.0, -1.0);
  return vec4<f32>(max(shade(uv), vec3<f32>(0.0)), 1.0);
}
