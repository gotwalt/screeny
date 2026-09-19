//! The host-side frame container and the seam renderers plug into.

use std::ops::{Deref, DerefMut};
use std::time::Duration;

use screeny_proto::{Rgb888Frame, H, NBYTES, NPIX, W};

/// One 64x32 sRGB frame, heap allocated so it can be moved about cheaply.
///
/// Derefs to [`Rgb888Frame`] (`[u8; 6144]`), which is what
/// [`crate::encode::Encoder::encode`] and `screeny_proto::decode` take, so a
/// `&Frame` works wherever a `&Rgb888Frame` is wanted.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    px: Box<Rgb888Frame>,
}

impl Default for Frame {
    fn default() -> Self {
        Self::black()
    }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame({W}x{H}, {} colours)", self.distinct_colours())
    }
}

impl Deref for Frame {
    type Target = Rgb888Frame;
    fn deref(&self) -> &Rgb888Frame {
        &self.px
    }
}

impl DerefMut for Frame {
    fn deref_mut(&mut self) -> &mut Rgb888Frame {
        &mut self.px
    }
}

impl Frame {
    /// An all-black frame.
    #[must_use]
    pub fn black() -> Self {
        Frame {
            px: Box::new([0u8; NBYTES]),
        }
    }

    /// A frame of one colour.
    #[must_use]
    pub fn solid(c: [u8; 3]) -> Self {
        let mut f = Self::black();
        for p in 0..NPIX {
            f.set_at(p, c);
        }
        f
    }

    /// Take ownership of an existing pixel array.
    #[must_use]
    pub fn from_pixels(px: Box<Rgb888Frame>) -> Self {
        Frame { px }
    }

    /// Copy a 6144-byte RGB888 buffer in.
    ///
    /// # Errors
    ///
    /// Returns the length it was given if that is not [`NBYTES`].
    pub fn from_bytes(b: &[u8]) -> Result<Self, usize> {
        if b.len() != NBYTES {
            return Err(b.len());
        }
        let mut f = Self::black();
        f.px.copy_from_slice(b);
        Ok(f)
    }

    /// The raw pixels.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; NBYTES] {
        &self.px
    }

    /// The raw pixels, mutably.
    pub fn as_bytes_mut(&mut self) -> &mut [u8; NBYTES] {
        &mut self.px
    }

    /// Pixel at `(x, y)`. Out-of-range coordinates wrap; the panel is a fixed
    /// 64x32 and a renderer that walks off it has a bug, but not a panic.
    #[inline(always)]
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        self.at((y % H) * W + (x % W))
    }

    /// Set the pixel at `(x, y)`.
    #[inline(always)]
    pub fn set(&mut self, x: usize, y: usize, c: [u8; 3]) {
        self.set_at((y % H) * W + (x % W), c);
    }

    /// Pixel `p` in raster order.
    #[inline(always)]
    #[must_use]
    pub fn at(&self, p: usize) -> [u8; 3] {
        let o = p * 3;
        [self.px[o], self.px[o + 1], self.px[o + 2]]
    }

    /// Set pixel `p` in raster order.
    #[inline(always)]
    pub fn set_at(&mut self, p: usize, c: [u8; 3]) {
        let o = p * 3;
        self.px[o] = c[0];
        self.px[o + 1] = c[1];
        self.px[o + 2] = c[2];
    }

    /// How many distinct colours the frame uses. Drives the chooser's
    /// exact-by-construction shortcuts.
    #[must_use]
    pub fn distinct_colours(&self) -> usize {
        let mut keys: Vec<u32> = (0..NPIX).map(|p| crate::color::pack(self.at(p))).collect();
        keys.sort_unstable();
        keys.dedup();
        keys.len()
    }
}

/// One frame on its way to the panel, in whichever form the producer has it.
///
/// This is the type the push API takes ([`crate::Sender::send`],
/// [`crate::Link::send`]). It borrows, so handing a frame over costs nothing,
/// and it takes **slices** rather than fixed-size arrays because the systems
/// that embed this library keep their frames in `Vec`s.
///
/// ```
/// use screeny::Pixels;
///
/// let palette = [[0, 0, 0], [255, 40, 0]];
/// let indices = vec![0u8; 64 * 32];
/// let px = Pixels::indexed(&palette, &indices);
/// assert!(px.is_indexed());
/// ```
///
/// # Why indexed is worth the trouble
///
/// [`Pixels::Indexed`] of 32 colours or fewer goes on the wire **exactly**:
/// `PAL4_LZ`, `PAL8_LZ` or raw `PAL5` carry the producer's own palette and
/// its own indices, so every pixel the panel lights is `palette[index]`, with
/// no quantisation and no dither of ours on top of theirs. [`Pixels::Rgb`] of
/// more than 32 colours goes through the chooser and is lossy by definition -
/// there is no 33-colour frame that fits in 1464 bytes.
///
/// Note that a frame's *colour count* is what matters, not the palette
/// length: an indexed frame with a 64-entry palette that only uses 12 of them
/// is a 12-colour frame, but this library takes it at its word and encodes
/// all 64, because re-deriving the histogram would cost more than it saves.
/// Producers should hand over a tight palette.
#[derive(Debug, Clone, Copy)]
pub enum Pixels<'a> {
    /// [`NBYTES`] bytes of row-major, top-left-origin sRGB `R,G,B`.
    Rgb(&'a [u8]),
    /// Up to 256 sRGB colours and [`NPIX`] indices into them.
    Indexed {
        /// The colours, sRGB `R,G,B`. 32 or fewer is the exact path.
        palette: &'a [[u8; 3]],
        /// One index per pixel, raster order. [`NPIX`] of them.
        indices: &'a [u8],
    },
}

impl<'a> Pixels<'a> {
    /// Raw sRGB pixels. The slice must be [`NBYTES`] long.
    #[must_use]
    pub fn rgb(px: &'a [u8]) -> Self {
        Pixels::Rgb(px)
    }

    /// A palette and indices into it.
    #[must_use]
    pub fn indexed(palette: &'a [[u8; 3]], indices: &'a [u8]) -> Self {
        Pixels::Indexed { palette, indices }
    }

    /// True for [`Pixels::Indexed`].
    #[must_use]
    pub fn is_indexed(&self) -> bool {
        matches!(self, Pixels::Indexed { .. })
    }

    /// Distinct colours the producer offered: the palette length for an
    /// indexed frame, and `None` for an RGB one (counting those means walking
    /// 2048 pixels, which the encoder does anyway).
    #[must_use]
    pub fn palette_len(&self) -> Option<usize> {
        match self {
            Pixels::Rgb(_) => None,
            Pixels::Indexed { palette, .. } => Some(palette.len()),
        }
    }

    /// Check the shape **and**, for an indexed frame, that every index is
    /// inside the palette.
    ///
    /// One pass over 2048 bytes, so it is worth doing before anything that
    /// might discard the frame: an error the caller can fix should not depend
    /// on whether the pacing happened to keep that frame.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Frame`] or [`crate::Error::BadIndex`] - between them,
    /// every way a caller of [`crate::Sender::send`] can get a frame wrong.
    pub fn validate(&self) -> crate::Result<()> {
        self.check()?;
        if let Pixels::Indexed { palette, indices } = self {
            if let Some((pixel, &index)) = indices
                .iter()
                .enumerate()
                .find(|(_, i)| **i as usize >= palette.len())
            {
                return Err(crate::Error::BadIndex {
                    index,
                    pixel,
                    palette: palette.len(),
                });
            }
        }
        Ok(())
    }

    /// Check the shape alone: the lengths, and that the palette is a legal
    /// size. Cheap, and does not look at the indices; [`Pixels::validate`] is
    /// the complete check.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Frame`] with the sizes involved.
    pub fn check(&self) -> crate::Result<()> {
        use crate::Error;
        match self {
            Pixels::Rgb(px) if px.len() != NBYTES => Err(Error::Frame {
                what: "an RGB frame",
                got: px.len(),
                want: NBYTES,
            }),
            Pixels::Indexed { indices, .. } if indices.len() != NPIX => Err(Error::Frame {
                what: "an index plane",
                got: indices.len(),
                want: NPIX,
            }),
            Pixels::Indexed { palette, .. } if palette.is_empty() || palette.len() > 256 => {
                Err(Error::Frame {
                    what: "a palette",
                    got: palette.len(),
                    want: 256,
                })
            }
            _ => Ok(()),
        }
    }
}

impl<'a> From<&'a Frame> for Pixels<'a> {
    fn from(f: &'a Frame) -> Self {
        Pixels::Rgb(f.as_bytes())
    }
}

impl<'a> From<&'a Rgb888Frame> for Pixels<'a> {
    fn from(f: &'a Rgb888Frame) -> Self {
        Pixels::Rgb(f)
    }
}

/// Where a frame sits in a stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameTime {
    /// Frame counter since the stream started. Skipped frames are skipped
    /// numbers: this is wall-clock position, not "how many we sent".
    pub index: u64,
    /// Time since the stream started.
    pub elapsed: Duration,
    /// The rate the pacer is currently targeting, in frames per second.
    pub fps: f64,
}

impl FrameTime {
    /// Seconds since the stream started.
    #[must_use]
    pub fn secs(&self) -> f64 {
        self.elapsed.as_secs_f64()
    }
}

/// Anything that can produce frames for [`crate::Sender`].
///
/// This is the seam the demos crate (card 010) plugs into: a renderer is a
/// pure `fn(t) -> pixels` and knows nothing about codecs, sockets or pacing.
/// Rendering is into a caller-owned buffer so a 30 fps stream does not
/// allocate 6 KB per frame.
pub trait FrameSource {
    /// Render the frame for `t` into `out`.
    ///
    /// Return `false` to end the stream; the sender then sends its `FINAL`
    /// frame and stops. `out` is whatever the previous call left behind, so a
    /// source that only touches part of the frame gets persistence for free.
    fn render(&mut self, t: FrameTime, out: &mut Frame) -> bool;

    /// A short name, for `--verbose` output and stats.
    #[allow(clippy::unnecessary_literal_bound)] // implementors may borrow
    fn name(&self) -> &str {
        "frames"
    }
}

/// Adapts a closure into a [`FrameSource`].
pub struct FnSource<F> {
    f: F,
    name: String,
}

impl<F: FnMut(FrameTime, &mut Frame) -> bool> FnSource<F> {
    /// Wrap `f`.
    pub fn new(name: impl Into<String>, f: F) -> Self {
        FnSource {
            f,
            name: name.into(),
        }
    }
}

impl<F: FnMut(FrameTime, &mut Frame) -> bool> FrameSource for FnSource<F> {
    fn render(&mut self, t: FrameTime, out: &mut Frame) -> bool {
        (self.f)(t, out)
    }
    fn name(&self) -> &str {
        &self.name
    }
}

/// Reads raw 6144-byte RGB888 frames from a stream (`screeny pipe`).
///
/// A short read at a frame boundary ends the stream; a short read in the
/// middle of one is an error, because silently displaying half a frame is
/// worse than stopping.
pub struct RawReader<R> {
    r: R,
    buf: Vec<u8>,
    /// Set when the reader hit a partial frame, so the caller can report it.
    pub truncated: bool,
}

impl<R: std::io::Read> RawReader<R> {
    /// Wrap a reader.
    pub fn new(r: R) -> Self {
        RawReader {
            r,
            buf: vec![0u8; NBYTES],
            truncated: false,
        }
    }
}

impl<R: std::io::Read> FrameSource for RawReader<R> {
    fn render(&mut self, _t: FrameTime, out: &mut Frame) -> bool {
        let mut got = 0usize;
        while got < NBYTES {
            match self.r.read(&mut self.buf[got..]) {
                Ok(0) => {
                    self.truncated = got != 0;
                    return false;
                }
                Ok(n) => got += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.truncated = got != 0;
                    return false;
                }
            }
        }
        out.as_bytes_mut().copy_from_slice(&self.buf);
        true
    }

    fn name(&self) -> &'static str {
        "pipe"
    }
}
