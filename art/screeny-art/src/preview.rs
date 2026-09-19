//! CPU rendering of the panel's look, for snapshots and contact sheets. The
//! studio does the same thing (plus bloom and squint) in a shader.

use crate::color::{linear_to_srgb8, smoothstep, srgb8_to_linear};
use crate::frame::{H, W};

/// Dot diameter as a fraction of LED pitch.
pub const DOT: f32 = 0.66;

/// Draw `rgb` (`N * 3` sRGB bytes) as round dots on black, `scale` output pixels
/// per LED. Returns (width, height, RGBA bytes).
pub fn render_dots(rgb: &[u8], scale: usize) -> (usize, usize, Vec<u8>) {
    let (ow, oh) = (W * scale, H * scale);
    let aa = 1.0 / scale as f32;
    let mut out = Vec::with_capacity(ow * oh * 4);
    for oy in 0..oh {
        for ox in 0..ow {
            let (x, y) = (ox / scale, oy / scale);
            let dx = (ox % scale) as f32 + 0.5 - scale as f32 / 2.0;
            let dy = (oy % scale) as f32 + 0.5 - scale as f32 / 2.0;
            let d = (dx * dx + dy * dy).sqrt() / scale as f32;
            let mask = 1.0 - smoothstep(DOT / 2.0 - aa, DOT / 2.0 + aa, d);
            let i = (y * W + x) * 3;
            for k in 0..3 {
                out.push(linear_to_srgb8(srgb8_to_linear(rgb[i + k]) * mask));
            }
            out.push(255);
        }
    }
    (ow, oh, out)
}
