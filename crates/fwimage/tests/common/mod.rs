//! An ESP32 app image, built byte by byte, so a test can break exactly one
//! thing about it.
//!
//! The layout is esptool's and is the one `espflash save-image` produces:
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
//! The one thing these images are not is loadable: the segment data is
//! recognisable filler rather than Xtensa code. Nothing in `screeny-fwimage`
//! looks at it, and nothing in the firmware's staging path does either - the
//! bootloader is what loads an image, and that is card 241's problem and the
//! bootloader's.

use sha2::{Digest, Sha256};

pub const CHIP_ESP32: u16 = 0x0000;
pub const CHIP_ESP32C3: u16 = 0x0005;

/// One segment's worth of made-up content.
pub struct Segment {
    pub load_addr: u32,
    pub data: Vec<u8>,
}

/// Everything a test might want to vary.
pub struct Builder {
    pub magic: u8,
    pub chip_id: u16,
    pub hash_appended: bool,
    pub project: &'static str,
    pub version: &'static str,
    pub desc_magic: u32,
    pub segments: Vec<Segment>,
}

impl Default for Builder {
    fn default() -> Self {
        Builder {
            magic: screeny_fwimage::IMAGE_MAGIC,
            chip_id: CHIP_ESP32,
            hash_appended: true,
            project: screeny_fwimage::PROJECT_NAME,
            version: "0.6.0",
            desc_magic: screeny_fwimage::APP_DESC_MAGIC,
            segments: vec![
                Segment {
                    load_addr: 0x3F40_0020,
                    // 1 KB: the descriptor plus filler, which is the shape of
                    // a real DROM segment.
                    data: vec![0; 1024],
                },
                // Over two sectors, so the good image spans three of them and
                // a test can talk about "the first sector" meaningfully.
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
    pub fn good() -> Self {
        Self::default()
    }

    /// The whole image, ready to be handed to [`screeny_fwimage::Scan`].
    pub fn build(mut self) -> Vec<u8> {
        // The app descriptor goes at the start of segment 0.
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

        // esptool pads so the checksum is the last byte of a 16-byte block.
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

/// Feed an image to a scan in `chunk` byte pieces, the way the staging loop
/// does, and return what the scan made of it.
pub fn scan_in_chunks(
    image: &[u8],
    slot_len: u32,
    chunk: usize,
) -> Result<screeny_fwimage::Image, screeny_device_api::FirmwareError> {
    let mut scan = screeny_fwimage::Scan::new(slot_len);
    for piece in image.chunks(chunk.max(1)) {
        // The staging loop pushes and *then* asks whether the front is good
        // enough to start erasing; mirroring that here keeps the two honest.
        scan.push(piece)?;
        scan.check_front()?;
    }
    scan.finish()
}

/// Two megabytes: `ota_0` and `ota_1` in `firmware/partitions.csv`.
pub const SLOT: u32 = 0x20_0000;
