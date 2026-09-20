//! An ESP32 app image, built byte by byte, so a test can break exactly one
//! thing about it.
//!
//! Feature `build`, on by default and **off in the firmware**
//! (`default-features = false`): it is the only thing here that needs `alloc`,
//! and a device has no reason to be able to make an image.
//!
//! It exists so that there is one builder rather than three. `crates/fwimage`'s
//! own tests use it to prove each check fires; `screeny-probe` uses it to send
//! the device a wrong-chip and a wrong-project image that are correct in every
//! other respect, which is what makes those probe rules mean anything; and a
//! developer can use it to make a deliberately broken image by hand.
//!
//! The layout is esptool's, and is what `espflash save-image` produces:
//!
//! ```text
//! 0      24-byte image header, byte 23 = "a SHA-256 is appended"
//! 24     segment 0 header (load address, length)
//! 32     segment 0 data, which begins with esp_app_desc
//! ...    further segments
//! N      zero padding, so that the next byte is the last of a 16-byte block
//! N+p    the one-byte checksum: 0xEF XOR every byte of every segment's data
//! N+p+1  32 bytes of SHA-256 over 0..N+p+1
//! ```
//!
//! The one thing these images are not is *loadable*: the segment data is
//! recognisable filler, not Xtensa code. Nothing in this crate looks at it and
//! nothing in the firmware's staging path does either - loading an image is
//! the bootloader's job, and making one bootable is card 241's.

use alloc::vec;
use alloc::vec::Vec;

use sha2::{Digest, Sha256};

use crate::{APP_DESC_MAGIC, CHIP_ID_ESP32, IMAGE_MAGIC, PROJECT_NAME};

/// `chip_id` for the ESP32-C3: a real chip, a real image, and one this device
/// must never run.
pub const CHIP_ID_ESP32C3: u16 = 0x0005;

/// One segment's worth of made-up content.
pub struct Segment {
    /// Where the ROM loader would put it. Never used by anything here.
    pub load_addr: u32,
    /// The bytes.
    pub data: Vec<u8>,
}

/// Everything a caller might want to vary about an image.
pub struct Builder {
    /// Header byte 0. [`IMAGE_MAGIC`] unless you are breaking check 1.
    pub magic: u8,
    /// Header bytes 12..14.
    pub chip_id: u16,
    /// Header byte 23.
    pub hash_appended: bool,
    /// `esp_app_desc.project_name`.
    pub project: &'static str,
    /// `esp_app_desc.version`.
    pub version: &'static str,
    /// `esp_app_desc.magic_word`.
    pub desc_magic: u32,
    /// The segments, in order. Segment 0 must be at least 256 bytes: the app
    /// descriptor is written over its front.
    pub segments: Vec<Segment>,
}

impl Default for Builder {
    /// A good image of this device's own shape: two segments, the first
    /// carrying the descriptor, and the whole thing a little over three
    /// sectors so that "the first sector" means something.
    fn default() -> Self {
        Builder {
            magic: IMAGE_MAGIC,
            chip_id: CHIP_ID_ESP32,
            hash_appended: true,
            project: PROJECT_NAME,
            version: "0.6.0",
            desc_magic: APP_DESC_MAGIC,
            segments: vec![
                Segment {
                    load_addr: 0x3F40_0020,
                    // 1 KB: the descriptor plus filler, which is the shape of
                    // a real DROM segment.
                    data: vec![0; 1024],
                },
                Segment {
                    load_addr: 0x4008_0000,
                    data: (0..8192u32).map(|i| (i % 251) as u8).collect(),
                },
            ],
        }
    }
}

fn field32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let b = s.as_bytes();
    out[..b.len()].copy_from_slice(b);
    out
}

impl Builder {
    /// A good image of this device's own shape.
    #[must_use]
    pub fn good() -> Self {
        Self::default()
    }

    /// A good image, but built for an ESP32-C3. Correct in every other
    /// respect, including its checksum and its appended hash - which is the
    /// point: only check 2 can tell it apart from one we should run.
    #[must_use]
    pub fn wrong_chip() -> Self {
        Builder {
            chip_id: CHIP_ID_ESP32C3,
            ..Self::default()
        }
    }

    /// A good ESP32 image of somebody else's project. Also correct in every
    /// other respect.
    #[must_use]
    pub fn wrong_project() -> Self {
        Builder {
            project: "esp-idf-blink",
            ..Self::default()
        }
    }

    /// The whole image, ready for [`crate::Scan`].
    ///
    /// # Panics
    ///
    /// If segment 0 is shorter than the 256-byte app descriptor, or if there
    /// are more than 255 segments - both of which are the caller asking for an
    /// image that could not exist rather than for a broken one.
    #[must_use]
    pub fn build(mut self) -> Vec<u8> {
        // The app descriptor goes at the start of segment 0, which is where
        // the linker script puts it in a real build.
        if let Some(seg) = self.segments.first_mut() {
            assert!(seg.data.len() >= 256, "segment 0 must hold esp_app_desc");
            let mut desc = [0u8; 256];
            desc[0..4].copy_from_slice(&self.desc_magic.to_le_bytes());
            desc[16..48].copy_from_slice(&field32(self.version));
            desc[48..80].copy_from_slice(&field32(self.project));
            desc[80..96].copy_from_slice(&field32("12:00:00")[..16]);
            seg.data[..256].copy_from_slice(&desc);
        }

        let mut out = Vec::new();
        let mut header = [0u8; 24];
        header[0] = self.magic;
        header[1] = u8::try_from(self.segments.len()).expect("too many segments");
        header[2] = 0x02; // SPI mode DIO
        header[3] = 0x2F; // 40 MHz, 8 MB
        header[4..8].copy_from_slice(&0x4008_0000u32.to_le_bytes());
        header[12..14].copy_from_slice(&self.chip_id.to_le_bytes());
        header[23] = u8::from(self.hash_appended);
        out.extend_from_slice(&header);

        let mut checksum = 0xEFu8;
        for seg in &self.segments {
            out.extend_from_slice(&seg.load_addr.to_le_bytes());
            out.extend_from_slice(&u32::try_from(seg.data.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&seg.data);
            for b in &seg.data {
                checksum ^= *b;
            }
        }

        // esptool seeks so that the checksum is the last byte of a 16-byte
        // block, and the bytes it skips read as zero.
        let end = (out.len() + 1 + 15) & !15;
        out.resize(end - 1, 0);
        out.push(checksum);

        if self.hash_appended {
            let mut h = Sha256::new();
            h.update(&out);
            out.extend_from_slice(&h.finalize());
        }
        out
    }
}
