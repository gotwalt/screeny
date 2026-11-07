//! The float methods `core` does not have.
//!
//! With the `std` feature these are never reached: an inherent method beats a
//! trait method of the same name, so a host build calls exactly what it called
//! before this crate existed and the Studio's pictures do not move by a bit.
//! Without it (the firmware) they are `libm`'s.

/// `std`'s `f32` methods, for `no_std`.
#[allow(dead_code)]
pub(crate) trait F32Ext: Sized {
    fn sin(self) -> f32;
    fn cos(self) -> f32;
    fn sin_cos(self) -> (f32, f32);
    fn atan2(self, x: f32) -> f32;
    fn sqrt(self) -> f32;
    fn powi(self, n: i32) -> f32;
    fn powf(self, n: f32) -> f32;
    fn exp(self) -> f32;
    fn floor(self) -> f32;
    fn ceil(self) -> f32;
    fn round(self) -> f32;
    fn fract(self) -> f32;
    fn hypot(self, other: f32) -> f32;
    fn rem_euclid(self, rhs: f32) -> f32;
}

impl F32Ext for f32 {
    fn sin(self) -> f32 {
        libm::sinf(self)
    }
    fn cos(self) -> f32 {
        libm::cosf(self)
    }
    fn sin_cos(self) -> (f32, f32) {
        libm::sincosf(self)
    }
    fn atan2(self, x: f32) -> f32 {
        libm::atan2f(self, x)
    }
    fn sqrt(self) -> f32 {
        libm::sqrtf(self)
    }
    fn powi(self, n: i32) -> f32 {
        let (mut base, mut e, mut out) = (self, n.unsigned_abs(), 1.0f32);
        while e > 0 {
            if e & 1 == 1 {
                out *= base;
            }
            base *= base;
            e >>= 1;
        }
        if n < 0 {
            1.0 / out
        } else {
            out
        }
    }
    fn powf(self, n: f32) -> f32 {
        libm::powf(self, n)
    }
    fn exp(self) -> f32 {
        libm::expf(self)
    }
    fn floor(self) -> f32 {
        libm::floorf(self)
    }
    fn ceil(self) -> f32 {
        libm::ceilf(self)
    }
    fn round(self) -> f32 {
        libm::roundf(self)
    }
    fn fract(self) -> f32 {
        self - libm::truncf(self)
    }
    fn hypot(self, other: f32) -> f32 {
        libm::hypotf(self, other)
    }
    fn rem_euclid(self, rhs: f32) -> f32 {
        let r = libm::fmodf(self, rhs);
        if r < 0.0 {
            r + libm::fabsf(rhs)
        } else {
            r
        }
    }
}

/// `std`'s `f64` methods, for `no_std`.
#[allow(dead_code)]
pub(crate) trait F64Ext: Sized {
    fn floor(self) -> f64;
}

impl F64Ext for f64 {
    fn floor(self) -> f64 {
        libm::floor(self)
    }
}

#[cfg(test)]
mod tests {
    use super::F32Ext;

    /// The device goes through these and the host does not, so nothing else
    /// would notice one of them being wrong.
    #[test]
    fn the_no_std_maths_agrees_with_std() {
        for i in -400..400 {
            let x = i as f32 * 1.37;
            let close = |a: f32, b: f32| assert!((a - b).abs() <= 1e-4 * b.abs().max(1.0), "{x}: {a} against {b}");
            close(F32Ext::rem_euclid(x, 360.0), x.rem_euclid(360.0));
            close(F32Ext::sin(x.to_radians()), x.to_radians().sin());
            close(F32Ext::fract(x), x.fract());
            close(F32Ext::powi(1.0 + x / 1000.0, 3), (1.0 + x / 1000.0).powi(3));
            close(F32Ext::powi(1.0 + x / 1000.0, -2), (1.0 + x / 1000.0).powi(-2));
            close(F32Ext::floor(x), x.floor());
        }
    }
}
