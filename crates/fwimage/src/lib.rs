//! What makes an ESP32 app image one *this* device will run.
//!
//! `esp-bootloader-esp-idf` will happily flip `otadata` to a slot holding an
//! ESP32-C3 build, somebody else's project, or half an image - it never looks
//! (research 006, conclusion 3). The gate has to be ours, and research 006
//! section 5 names it: five checks, cheapest first, run against the image
//! **before** anything can boot it.
//!
//! This crate is that gate, written once. The firmware runs it on the bytes as
//! they come off the socket (card 240), `crates/sim` runs it on the bytes it
//! reads and discards, and the tests here run it on images built byte by byte
//! and then deliberately broken. Three callers, one answer - which is the
//! whole reason it is a crate and not a function in `firmware/src/`.
//!
//! ## The checks, and the order
//!
//! | # | check | [`FirmwareError`] |
//! |---|---|---|
//! | 1 | byte 0 is the ESP image magic `0xE9` | [`FirmwareError::BadMagic`] |
//! | 2 | the header's chip id is ESP32 (`0x0000`) | [`FirmwareError::WrongChip`] |
//! | 3 | the header says a SHA-256 is appended (byte 23) | [`FirmwareError::BadSha256`] |
//! | 4 | `esp_app_desc.project_name` is `screeny-fw` | [`FirmwareError::WrongProject`] |
//! | 5 | the segment table walks and the one-byte XOR checksum is right | [`FirmwareError::BadChecksum`] |
//! | 6 | the appended SHA-256 matches the bytes it covers | [`FirmwareError::BadSha256`] |
//!
//! Six rows for 006's five checks because 006 folds "the header says a hash is
//! appended" into the hash check: an image without one cannot be verified at
//! all, so refusing it *is* the SHA-256 check failing, and it is reported as
//! such. The two rows are kept apart here because they fail at very different
//! moments - the flag is known from the first 24 bytes and the digest only at
//! the end - and the first four are all decidable from the first 4,096 bytes.
//! **That is the property the whole design rests on**: a wrong-chip or
//! wrong-project upload is refused before a single sector has been erased.
//!
//! ## What it is not
//!
//! It is not a loader and it does not hold the image. [`Scan`] is 240-odd
//! bytes: a copy of the first 80 (the header and the app descriptor), a
//! segment cursor, a running XOR, and a SHA-256 state. Feed it the upload in
//! whatever pieces it arrives in and it answers at the end. Nothing here
//! allocates, reads a clock or touches I/O.

#![no_std]
#![forbid(unsafe_code)]

use screeny_device_api::FirmwareError;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// The image format, as constants
// ---------------------------------------------------------------------------

/// One 4 KB flash sector: the unit an upload is staged in, so core 1 is
/// stalled for one sector erase (~50 ms) at a time rather than one 64 KB block
/// or one whole slot (research 006 section 5).
pub const SECTOR: usize = 4096;

/// The ESP image header, in bytes.
pub const HEADER_LEN: usize = 24;

/// The ESP image magic, header byte 0.
pub const IMAGE_MAGIC: u8 = 0xE9;

/// `chip_id` for the ESP32, header bytes 12..14 little-endian.
pub const CHIP_ID_ESP32: u16 = 0x0000;

/// Most segments an ESP image may declare. `esp-bootloader-esp-idf` refuses
/// more (`partitions.rs` line 684) and so does the ROM loader.
pub const MAX_SEGMENTS: u8 = 16;

/// Where `esp_app_desc` sits: immediately after the image header and the first
/// segment's own 8-byte header.
pub const APP_DESC_OFFSET: usize = HEADER_LEN + 8;

/// `esp_app_desc.magic_word`.
pub const APP_DESC_MAGIC: u32 = 0xABCD_5432;

/// `esp_app_desc.version`, relative to [`APP_DESC_OFFSET`].
const DESC_VERSION: usize = 16;
/// `esp_app_desc.project_name`, relative to [`APP_DESC_OFFSET`].
const DESC_PROJECT: usize = 48;
/// Both of those fields are 32 bytes, NUL-padded.
const DESC_FIELD: usize = 32;

/// The `esp_app_desc.project_name` this device accepts, and nothing else.
///
/// `firmware/src/main.rs` uses the no-argument `esp_app_desc!`, which takes
/// `CARGO_PKG_NAME`, and `firmware/Cargo.toml` names the package `screeny-fw`.
pub const PROJECT_NAME: &str = "screeny-fw";

/// Bytes of appended SHA-256.
pub const HASH_LEN: usize = 32;

/// How much of the front of an image this crate keeps a copy of: the header,
/// the first segment header, and the app descriptor's magic, version and
/// project name.
const HEAD_LEN: usize = APP_DESC_OFFSET + DESC_PROJECT + DESC_FIELD;

const _: () = assert!(HEAD_LEN == 112);
const _: () = assert!(HEAD_LEN < SECTOR, "the head must fit the first chunk");

/// The seed the ESP image checksum starts from. Every byte of every segment's
/// *data* is XORed into it, and the result is the last byte of the image
/// before the appended hash.
const CHECKSUM_SEED: u8 = 0xEF;

// ---------------------------------------------------------------------------
// Where a chunk is allowed to land
// ---------------------------------------------------------------------------

/// Refuse a write that would fall outside the slot, or that is not what the
/// staging loop promises to do.
///
/// The `FlashRegion` this ends up calling is partition-relative and
/// bounds-checked by `esp-bootloader-esp-idf` itself, so this is the second
/// lock on the same door rather than the only one - but it is the one a host
/// test can drive, and it is the one that catches a *caller* bug (an offset
/// that has drifted, a chunk that is not sector aligned) before the ROM is
/// asked to erase anything.
///
/// # Errors
///
/// [`FirmwareError::TooLarge`] when `offset + len` runs past `slot_len`, or
/// [`FirmwareError::Flash`] when the offset is not sector aligned or the
/// length is longer than one sector - neither of which a correct staging loop
/// can produce, which is exactly why they are refused rather than fixed up.
pub fn plan_write(slot_len: u32, offset: u32, len: usize) -> Result<(), FirmwareError> {
    if !(offset as usize).is_multiple_of(SECTOR) || len > SECTOR || len == 0 {
        return Err(FirmwareError::Flash);
    }
    let end = (offset as u64) + (len as u64);
    if end > u64::from(slot_len) {
        return Err(FirmwareError::TooLarge);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The app descriptor's version string
// ---------------------------------------------------------------------------

/// `esp_app_desc.version`: 32 NUL-padded bytes, which for our own builds is
/// whatever `firmware/Cargo.toml` says.
///
/// Kept as bytes rather than as a `str` because it comes off a socket: an
/// image built by somebody else can put anything in there, and a lossy
/// conversion of 32 arbitrary bytes into a log line is how a serial console
/// ends up full of control characters.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Version([u8; DESC_FIELD]);

impl Version {
    /// The version as text, or `None` when it is not ASCII.
    ///
    /// ASCII and not UTF-8 on purpose: this is printed on a serial log and
    /// may be shown on a 4x6 font, and "is every byte a printable ASCII
    /// character" is the question both of those are really asking.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        let len = self.0.iter().position(|b| *b == 0).unwrap_or(DESC_FIELD);
        let s = &self.0[..len];
        if s.iter().all(|b| (0x20..0x7f).contains(b)) {
            core::str::from_utf8(s).ok()
        } else {
            None
        }
    }
}

impl core::fmt::Debug for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.as_str() {
            Some(s) => write!(f, "{s:?}"),
            None => write!(f, "<{} non-ascii bytes>", DESC_FIELD),
        }
    }
}

// ---------------------------------------------------------------------------
// What a complete, accepted image turned out to be
// ---------------------------------------------------------------------------

/// The answer when every check passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    /// Length of the image proper, appended hash included. This is the number
    /// the bootloader works from; anything after it on flash is never read.
    pub len: u32,
    /// Bytes the uploader sent *past* the end of the image.
    ///
    /// Not an error: the bootloader takes the length from the header and the
    /// segment table, so trailing bytes are ignored. It is reported because an
    /// upload with a tail is almost always a mistake worth a log line.
    pub trailing: u32,
    /// The appended SHA-256, which matched what the bytes hash to.
    pub digest: [u8; HASH_LEN],
    /// `esp_app_desc.version`.
    pub version: Version,
    /// Segments the image declared.
    pub segments: u8,
}

// ---------------------------------------------------------------------------
// The scanner
// ---------------------------------------------------------------------------

/// Which part of the image the cursor is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// The 24-byte image header.
    Header,
    /// One 8-byte segment header.
    SegmentHeader,
    /// `remaining` bytes of one segment's data.
    SegmentData,
    /// Zero padding up to the last byte of a 16-byte block.
    Padding,
    /// The one checksum byte, which is that last byte.
    Checksum,
    /// The appended SHA-256.
    Hash,
    /// Past the end of the image.
    Trailing,
}

/// Research 006 section 5's checks, run on an image that is arriving.
///
/// Feed it with [`push`](Self::push) in whatever pieces the transport hands
/// over - they need not be aligned to anything - and close it with
/// [`finish`](Self::finish). The first four checks fail on the first call that
/// carries the bytes they need, which for any real upload is the first one, so
/// a caller that stages as it goes can refuse a wrong-chip image **before it
/// erases a sector**.
pub struct Scan {
    /// Longest image this slot can hold.
    slot_len: u32,
    /// Bytes fed so far.
    pos: u32,
    phase: Phase,
    /// A copy of the first [`HEAD_LEN`] bytes, for checks 1-4.
    head: [u8; HEAD_LEN],
    /// How much of `head` is filled.
    head_len: usize,
    /// Segment headers still to come.
    segments_left: u8,
    /// Segments the header declared.
    segments: u8,
    /// Bytes of the current segment header collected so far.
    seg_hdr: [u8; 8],
    seg_hdr_len: usize,
    /// Bytes of the current segment's data still to come.
    seg_remaining: u32,
    /// The running XOR of every segment data byte.
    checksum: u8,
    /// Where the image ends and the appended hash begins. Known once the
    /// segment walk is over.
    hash_start: Option<u32>,
    /// The appended digest, as it arrives.
    digest: [u8; HASH_LEN],
    digest_len: usize,
    /// SHA-256 over `0..hash_start`.
    sha: Sha256,
    /// Set by the first failure; every later call returns the same one, so a
    /// caller that keeps pushing gets a stable answer.
    failed: Option<FirmwareError>,
}

impl Scan {
    /// A scanner for a slot of `slot_len` bytes.
    #[must_use]
    pub fn new(slot_len: u32) -> Self {
        Scan {
            slot_len,
            pos: 0,
            phase: Phase::Header,
            head: [0; HEAD_LEN],
            head_len: 0,
            segments_left: 0,
            segments: 0,
            seg_hdr: [0; 8],
            seg_hdr_len: 0,
            seg_remaining: 0,
            checksum: CHECKSUM_SEED,
            hash_start: None,
            digest: [0; HASH_LEN],
            digest_len: 0,
            sha: Sha256::new(),
            failed: None,
        }
    }

    /// Bytes fed so far.
    #[must_use]
    pub fn seen(&self) -> u32 {
        self.pos
    }

    /// The image's total length, once the segment walk has settled it.
    ///
    /// A caller can use it for a progress figure; before the walk is over
    /// there is no honest answer, because the length is the sum of the segment
    /// headers and they are spread through the image.
    #[must_use]
    pub fn image_len(&self) -> Option<u32> {
        self.hash_start.map(|h| h + HASH_LEN as u32)
    }

    /// Whatever the first failure was, if there has been one.
    #[must_use]
    pub fn error(&self) -> Option<FirmwareError> {
        self.failed
    }

    /// Feed the next piece of the upload.
    ///
    /// # Errors
    ///
    /// The first check this chunk breaks. Once a scan has failed every later
    /// call returns the same error, so a caller may either stop at once or
    /// keep going and read it off [`finish`](Self::finish).
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), FirmwareError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        match self.feed(bytes) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.failed = Some(e);
                Err(e)
            }
        }
    }

    fn feed(&mut self, mut bytes: &[u8]) -> Result<(), FirmwareError> {
        // The whole upload has to fit the slot. Checked on the stream and not
        // only on `Content-Length`, because a chunked or lying sender is
        // exactly the case a bound is for.
        let total = u64::from(self.pos) + bytes.len() as u64;
        if total > u64::from(self.slot_len) {
            return Err(FirmwareError::TooLarge);
        }

        while !bytes.is_empty() {
            let n = self.step(bytes)?;
            debug_assert!(n > 0 && n <= bytes.len());
            bytes = &bytes[n..];
        }
        Ok(())
    }

    /// Consume as much of the front of `bytes` as the current phase wants, and
    /// say how much that was.
    ///
    /// Every arm works out `n` first and only then moves the phase on, because
    /// where the padding and the checksum byte fall depends on the position
    /// **after** this step - and [`Scan::pos`] is not advanced until the end.
    fn step(&mut self, bytes: &[u8]) -> Result<usize, FirmwareError> {
        // Everything before the appended hash is hashed; the hash itself and
        // anything after it is not.
        let n = match self.phase {
            Phase::Header => {
                let n = self.take_head(bytes, HEADER_LEN);
                let after = self.pos + n as u32;
                if self.head_len >= HEADER_LEN {
                    self.check_header()?;
                    self.phase = if self.segments_left == 0 {
                        self.begin_padding(after)
                    } else {
                        Phase::SegmentHeader
                    };
                }
                n
            }
            Phase::SegmentHeader => {
                let want = 8 - self.seg_hdr_len;
                let n = want.min(bytes.len());
                let after = self.pos + n as u32;
                self.seg_hdr[self.seg_hdr_len..self.seg_hdr_len + n]
                    .copy_from_slice(&bytes[..n]);
                self.seg_hdr_len += n;
                // The head copy runs through the first segment header and into
                // the app descriptor, so it is fed from here too.
                self.fill_head(&bytes[..n]);
                if self.seg_hdr_len == 8 {
                    self.seg_hdr_len = 0;
                    let data_len = u32::from_le_bytes([
                        self.seg_hdr[4],
                        self.seg_hdr[5],
                        self.seg_hdr[6],
                        self.seg_hdr[7],
                    ]);
                    // The ROM loader's own rules: a segment is a whole number
                    // of words and cannot be bigger than the address space it
                    // is loaded into. A table that breaks them is a broken
                    // image, which is the checksum's arm.
                    if !data_len.is_multiple_of(4) || data_len >= 16 * 1024 * 1024 {
                        return Err(FirmwareError::BadChecksum);
                    }
                    self.segments_left -= 1;
                    self.seg_remaining = data_len;
                    self.phase = if data_len != 0 {
                        Phase::SegmentData
                    } else if self.segments_left == 0 {
                        self.begin_padding(after)
                    } else {
                        Phase::SegmentHeader
                    };
                }
                n
            }
            Phase::SegmentData => {
                let n = (self.seg_remaining as usize).min(bytes.len());
                let after = self.pos + n as u32;
                let chunk = &bytes[..n];
                // The ESP image checksum: 0xEF XOR every byte of every
                // segment's data, and nothing else.
                for b in chunk {
                    self.checksum ^= *b;
                }
                self.fill_head(chunk);
                self.seg_remaining -= n as u32;
                if self.seg_remaining == 0 {
                    self.phase = if self.segments_left == 0 {
                        self.begin_padding(after)
                    } else {
                        Phase::SegmentHeader
                    };
                }
                n
            }
            Phase::Padding => {
                // `begin_padding` set `seg_remaining` to the pad length.
                let n = (self.seg_remaining as usize).min(bytes.len());
                self.seg_remaining -= n as u32;
                if self.seg_remaining == 0 {
                    self.phase = Phase::Checksum;
                }
                n
            }
            Phase::Checksum => {
                if bytes[0] != self.checksum {
                    return Err(FirmwareError::BadChecksum);
                }
                // The image proper ends here; the hash, if there is one,
                // starts at the next byte.
                self.hash_start = Some(self.pos + 1);
                self.phase = Phase::Hash;
                1
            }
            Phase::Hash => {
                let want = HASH_LEN - self.digest_len;
                let n = want.min(bytes.len());
                self.digest[self.digest_len..self.digest_len + n].copy_from_slice(&bytes[..n]);
                self.digest_len += n;
                if self.digest_len == HASH_LEN {
                    self.phase = Phase::Trailing;
                }
                n
            }
            Phase::Trailing => bytes.len(),
        };

        // Hash everything up to, but not including, the appended digest.
        if matches!(
            self.phase,
            Phase::Header
                | Phase::SegmentHeader
                | Phase::SegmentData
                | Phase::Padding
                | Phase::Checksum
        ) || self.hash_start == Some(self.pos + n as u32)
        {
            self.sha.update(&bytes[..n]);
        }
        self.pos += n as u32;
        Ok(n)
    }

    /// Copy the front of the image into [`Scan::head`] while there is room,
    /// and run check 4 the moment the app descriptor is complete.
    fn take_head(&mut self, bytes: &[u8], want_to: usize) -> usize {
        let n = (want_to - self.head_len).min(bytes.len());
        self.fill_head(&bytes[..n]);
        n
    }

    fn fill_head(&mut self, bytes: &[u8]) {
        if self.head_len >= HEAD_LEN {
            return;
        }
        let n = (HEAD_LEN - self.head_len).min(bytes.len());
        self.head[self.head_len..self.head_len + n].copy_from_slice(&bytes[..n]);
        self.head_len += n;
    }

    /// Checks 1, 2 and 3, from the 24 bytes of the image header.
    fn check_header(&mut self) -> Result<(), FirmwareError> {
        let h = &self.head;
        // 1: the ESP image magic.
        if h[0] != IMAGE_MAGIC {
            return Err(FirmwareError::BadMagic);
        }
        // 2: the chip. This is what stops an ESP32-C3 build - which is a
        // perfectly well-formed image with a perfectly good hash - from ever
        // reaching otadata.
        if u16::from_le_bytes([h[12], h[13]]) != CHIP_ID_ESP32 {
            return Err(FirmwareError::WrongChip);
        }
        // 3: an image with no appended hash cannot be checked at all, so
        // refusing it *is* the SHA-256 check failing.
        if h[23] == 0 {
            return Err(FirmwareError::BadSha256);
        }
        if h[1] > MAX_SEGMENTS {
            return Err(FirmwareError::BadChecksum);
        }
        self.segments = h[1];
        self.segments_left = h[1];
        Ok(())
    }

    /// Check 4, from the app descriptor. Called once the head copy is full,
    /// which is 112 bytes in.
    fn check_app_desc(&self) -> Result<Version, FirmwareError> {
        let d = &self.head[APP_DESC_OFFSET..];
        if u32::from_le_bytes([d[0], d[1], d[2], d[3]]) != APP_DESC_MAGIC {
            return Err(FirmwareError::WrongProject);
        }
        let name = &d[DESC_PROJECT..DESC_PROJECT + DESC_FIELD];
        let len = name.iter().position(|b| *b == 0).unwrap_or(DESC_FIELD);
        if &name[..len] != PROJECT_NAME.as_bytes() {
            return Err(FirmwareError::WrongProject);
        }
        let mut v = [0u8; DESC_FIELD];
        v.copy_from_slice(&d[DESC_VERSION..DESC_VERSION + DESC_FIELD]);
        Ok(Version(v))
    }

    /// The segment walk is over: work out how much zero padding stands between
    /// `pos` and the checksum byte, which is the last byte of a 16-byte block.
    fn begin_padding(&mut self, pos: u32) -> Phase {
        // esptool seeks to `(pos + 1 + 15) & !15` and writes the checksum as
        // that block's last byte, which is what `esp-bootloader-esp-idf`
        // computes too (`partitions.rs` line 706).
        let end = (pos.saturating_add(1 + 15)) & !15;
        self.seg_remaining = end.saturating_sub(pos + 1);
        if self.seg_remaining == 0 {
            Phase::Checksum
        } else {
            Phase::Padding
        }
    }

    /// Every check, and the answer.
    ///
    /// # Errors
    ///
    /// The first check that failed - during [`push`](Self::push) or here.
    /// A body that stops before the image the header describes fails as
    /// [`FirmwareError::BadSha256`]: there is no digest to compare, which is
    /// research 006's own account of what a truncated upload dies of.
    pub fn finish(&self) -> Result<Image, FirmwareError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        // An upload with no header at all - an empty body, the one probe rule
        // 23 sends - has not got as far as failing check 1, so it is check 1
        // that it fails: there is no `0xE9` there.
        if self.head_len < HEADER_LEN {
            return Err(FirmwareError::BadMagic);
        }
        // Stopped after the header but before the descriptor: truncated, which
        // is the hash's arm, not the project name's. Saying `wrong_project`
        // about a body that simply stopped would send somebody looking in the
        // wrong place.
        if self.head_len < HEAD_LEN {
            return Err(FirmwareError::BadSha256);
        }
        // Check 4, last of the cheap four because it is the one that needs the
        // most bytes to have arrived (112) - but still decided from the first
        // sector, which is what lets the caller refuse before erasing.
        let version = self.check_app_desc()?;
        if self.phase != Phase::Trailing {
            // The image parsed but the body stopped inside it.
            return Err(FirmwareError::BadSha256);
        }
        // 6: the appended digest against the bytes it covers.
        //
        // The hasher is **cloned** rather than consumed, so that this can take
        // `&self`: the firmware's `Upload` has a `Drop` impl - it is what gives
        // the panel and the flash back - and a type with one can never have a
        // field moved out of it. `Sha256` is `Clone` and its state is 112
        // bytes.
        let computed: [u8; HASH_LEN] = self.sha.clone().finalize().into();
        if computed != self.digest {
            return Err(FirmwareError::BadSha256);
        }
        let hash_start = self.hash_start.unwrap_or(self.pos);
        let len = hash_start + HASH_LEN as u32;
        Ok(Image {
            len,
            trailing: self.pos.saturating_sub(len),
            digest: self.digest,
            version,
            segments: self.segments,
        })
    }

    /// Run checks 1-4 as soon as the first sector has arrived, without
    /// finishing the scan.
    ///
    /// This is what makes "refused before a single sector was erased" true:
    /// the staging loop fills one 4,096-byte buffer, calls
    /// [`push`](Self::push) and then this, and only reaches the ROM's erase
    /// routine if both said yes.
    ///
    /// # Errors
    ///
    /// Checks 1-4. Says nothing at all - `Ok(())` - while fewer than
    /// [`HEAD_LEN`] bytes have arrived, because the app descriptor is not
    /// there yet; [`finish`](Self::finish) is where a body that short is
    /// refused.
    pub fn check_front(&self) -> Result<(), FirmwareError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        if self.head_len < HEAD_LEN {
            return Ok(());
        }
        self.check_app_desc().map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// Hashing bytes that are somewhere else
// ---------------------------------------------------------------------------

/// SHA-256 over bytes a caller supplies, for check 6 run a second time against
/// a copy of the image that is not the one the scanner saw.
///
/// The firmware uses it to hash the staged slot **back out of flash**: the
/// scan verified what came off the socket, and this verifies what came out of
/// the ROM's program routine, which is the half that catches a flash write
/// that went wrong. It lives here so that `sha2` is a dependency of one crate
/// and the firmware links a single copy of it.
pub struct Rehash(Sha256);

impl Rehash {
    /// A fresh hasher.
    #[must_use]
    pub fn new() -> Self {
        Rehash(Sha256::new())
    }

    /// Feed it the next piece.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// Whether what it hashed is what `expect` says.
    #[must_use]
    pub fn matches(self, expect: &[u8; HASH_LEN]) -> bool {
        let got: [u8; HASH_LEN] = self.0.finalize().into();
        &got == expect
    }
}

impl Default for Rehash {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Scan {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Scan")
            .field("pos", &self.pos)
            .field("phase", &self.phase)
            .field("segments_left", &self.segments_left)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}
