// Shared by every GPU patch. All colour here is LINEAR light; never gamma-encode
// in a shader, the pipeline does that once at the end.

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;

// OKLCH -> linear sRGB. l 0..1, c roughly 0..0.3, h in degrees. Out-of-gamut
// colours are clipped rather than chroma-reduced, so keep c moderate (<= 0.2)
// if hue accuracy matters.
fn oklch(l: f32, c: f32, h: f32) -> vec3<f32> {
  let a = c * cos(radians(h));
  let b = c * sin(radians(h));
  let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
  let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
  let s_ = l - 0.0894841775 * a - 1.2914855480 * b;
  let lms = vec3<f32>(l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
  return clamp(vec3<f32>(
    dot(lms, vec3<f32>(4.0767416621, -3.3077115913, 0.2309699292)),
    dot(lms, vec3<f32>(-1.2684380046, 2.6097574011, -0.3413193965)),
    dot(lms, vec3<f32>(-0.0041960863, -0.7034186147, 1.7076147010)),
  ), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn rot2(a: f32) -> mat2x2<f32> {
  let c = cos(a);
  let s = sin(a);
  return mat2x2<f32>(c, s, -s, c);
}

fn hash13(p: vec3<f32>) -> f32 {
  var q = fract(p * 0.1031);
  q += dot(q, q.zyx + 31.32);
  return fract((q.x + q.y) * q.z);
}
