//! Writing a decoded frame out as a 64x32 PNG.
//!
//! Deliberately 1:1 and deliberately *pre* panel model: a dump is for
//! diffing against what a sender thought it encoded, so it has to be the
//! bytes the decoder produced and nothing else. The LED-dot view is for
//! looking at; this is for `cmp`.

use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

use screeny_proto::{Rgb888Frame, H, W};

/// Write one frame to `path` as a 64x32 8-bit RGB PNG.
///
/// # Errors
///
/// Anything that stops the file being created or written.
pub fn write_png(path: &Path, frame: &Rgb888Frame) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = BufWriter::new(File::create(path)?);
    let mut enc = png::Encoder::new(file, W as u32, H as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(png_err)?;
    writer.write_image_data(frame).map_err(png_err)?;
    writer.finish().map_err(png_err)
}

/// A directory frames are numbered into: `frame-000123.png`.
#[derive(Debug, Clone)]
pub struct Dumper {
    dir: PathBuf,
    /// Write one frame in every `every`. 1 writes all of them.
    every: u32,
    seen: u32,
    written: u32,
}

impl Dumper {
    /// Create the directory and prepare to number frames into it.
    ///
    /// # Errors
    ///
    /// Anything that stops the directory being created.
    pub fn new(dir: impl Into<PathBuf>, every: u32) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Dumper {
            dir,
            every: every.max(1),
            seen: 0,
            written: 0,
        })
    }

    /// Offer a displayed frame. Writes it if it is the Nth.
    ///
    /// Returns the path written, if one was.
    ///
    /// # Errors
    ///
    /// Anything that stops the file being written.
    pub fn offer(&mut self, frame: &Rgb888Frame) -> io::Result<Option<PathBuf>> {
        let n = self.seen;
        self.seen += 1;
        if !n.is_multiple_of(self.every) {
            return Ok(None);
        }
        let path = self.dir.join(format!("frame-{n:06}.png"));
        write_png(&path, frame)?;
        self.written += 1;
        Ok(Some(path))
    }

    /// How many frames have been written.
    #[must_use]
    pub fn written(&self) -> u32 {
        self.written
    }
}

fn png_err(e: png::EncodingError) -> io::Error {
    match e {
        png::EncodingError::IoError(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use screeny_proto::NBYTES;
    use std::io::BufReader;

    #[test]
    fn a_dumped_frame_reads_back_pixel_for_pixel() {
        let dir = std::env::temp_dir().join(format!("screeny-sim-dump-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut frame = [0u8; NBYTES];
        for (i, b) in frame.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut d = Dumper::new(&dir, 3).unwrap();
        // 0 is written, 1 and 2 are not, 3 is.
        assert!(d.offer(&frame).unwrap().is_some());
        assert!(d.offer(&frame).unwrap().is_none());
        assert!(d.offer(&frame).unwrap().is_none());
        let path = d.offer(&frame).unwrap().expect("every third frame");
        assert_eq!(d.written(), 2);
        assert!(path.ends_with("frame-000003.png"));

        let decoder = png::Decoder::new(BufReader::new(File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (64, 32));
        assert_eq!(&buf[..info.buffer_size()], &frame[..]);

        fs::remove_dir_all(&dir).unwrap();
    }
}
