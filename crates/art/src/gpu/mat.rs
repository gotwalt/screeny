//! Just enough 4x4 matrix maths for mesh patches. Column-major, right-handed,
//! clip-space depth 0..1 (wgpu's convention, not OpenGL's -1..1).

pub type Mat4 = [[f32; 4]; 4];

pub const IDENTITY: Mat4 = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// `a * b`: applies `b` first.
pub fn mul(a: Mat4, b: Mat4) -> Mat4 {
    let mut m = [[0.0; 4]; 4];
    for (col, out) in m.iter_mut().enumerate() {
        for (row, v) in out.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[k][row] * b[col][k]).sum();
        }
    }
    m
}

pub fn translate(x: f32, y: f32, z: f32) -> Mat4 {
    let mut m = IDENTITY;
    m[3] = [x, y, z, 1.0];
    m
}

pub fn rot_x(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0, 0.0], [0.0, c, s, 0.0], [0.0, -s, c, 0.0], [0.0, 0.0, 0.0, 1.0]]
}

pub fn rot_y(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    [[c, 0.0, -s, 0.0], [0.0, 1.0, 0.0, 0.0], [s, 0.0, c, 0.0], [0.0, 0.0, 0.0, 1.0]]
}

pub fn rot_z(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    [[c, s, 0.0, 0.0], [-s, c, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]]
}

/// `fov_y` in radians. The panel's aspect is 2.0.
pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fov_y * 0.5).tan();
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, near * far / (near - far), 0.0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perspective_maps_near_and_far() {
        let p = perspective(1.0, 2.0, 0.5, 10.0);
        let depth = |z: f32| {
            let clip = mul(p, translate(0.0, 0.0, z))[3];
            clip[2] / clip[3]
        };
        assert!(depth(-0.5).abs() < 1e-5);
        assert!((depth(-10.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn rotations_compose() {
        let m = mul(rot_y(0.3), rot_y(-0.3));
        for (c, col) in m.iter().enumerate() {
            for (r, v) in col.iter().enumerate() {
                assert!((v - IDENTITY[c][r]).abs() < 1e-6);
            }
        }
    }
}
