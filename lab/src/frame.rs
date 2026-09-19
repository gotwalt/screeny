//! Host-side frame container.

pub use crate::dec::{Pixels, H, NBYTES, NPIX, W};

#[derive(Clone)]
pub struct Frame {
    pub px: Box<Pixels>,
}

impl Default for Frame {
    fn default() -> Self {
        Self::black()
    }
}

impl Frame {
    pub fn black() -> Self {
        Frame {
            px: Box::new([0u8; NBYTES]),
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        let o = (y * W + x) * 3;
        [self.px[o], self.px[o + 1], self.px[o + 2]]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, c: [u8; 3]) {
        let o = (y * W + x) * 3;
        self.px[o] = c[0];
        self.px[o + 1] = c[1];
        self.px[o + 2] = c[2];
    }

    #[inline]
    pub fn at(&self, p: usize) -> [u8; 3] {
        [self.px[p * 3], self.px[p * 3 + 1], self.px[p * 3 + 2]]
    }

    pub fn distinct_colours(&self) -> usize {
        let mut s = std::collections::HashSet::new();
        for p in 0..NPIX {
            s.insert(self.at(p));
        }
        s.len()
    }
}

/// A named sequence of frames.
pub struct Clip {
    pub name: &'static str,
    pub blurb: &'static str,
    pub frames: Vec<Frame>,
}
