//! Raymarched flight through a lattice. The whole piece is `lattice.wgsl`; this
//! file only names it and lists its parameters. Copy both to start a new
//! full-frame shader piece.

use crate::gpu::ShaderPiece;
use crate::piece::{param, ParamSpec, Piece, PieceDef};

pub const DEF: PieceDef = PieceDef {
    id: "lattice",
    name: "Lattice",
    blurb: "GPU, raymarched. Solids on true black, lit into the mid-to-bright range, supersampled for slow flight.",
    params: PARAMS,
    make,
};

// Order matters: the shader reads these as P(0), P(1), ...
const PARAMS: &[ParamSpec] = &[
    param("speed", "Speed (cells/s)", 0.0, 2.0, 0.01, 0.35),
    param("spacing", "Spacing", 1.0, 4.0, 0.01, 2.0),
    param("size", "Solid size", 0.1, 0.9, 0.01, 0.36),
    param("shape", "Sphere to box", 0.0, 1.0, 0.01, 0.0),
    param("depth", "Depth (cells)", 1.5, 8.0, 0.1, 3.2),
    param("hue", "Hue", 0.0, 360.0, 1.0, 40.0),
    param("spread", "Hue spread", 0.0, 360.0, 1.0, 140.0),
    param("samples", "Samples per axis", 1.0, 8.0, 1.0, 4.0),
];

fn make(seed: u64) -> Box<dyn Piece> {
    ShaderPiece::boxed("lattice", include_str!("lattice.wgsl"), PARAMS, seed)
}
