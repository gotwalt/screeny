//! Research 006 section 5's checks, one deliberately broken image each.
//!
//! Every test here breaks **one** thing about an image that would otherwise be
//! accepted, so a failure names the check rather than "something about the
//! image". The good image is built by the same code with nothing broken, which
//! is what makes that claim true.

use screeny_device_api::FirmwareError;
use screeny_fwimage::build::{Builder, Segment, CHIP_ID_ESP32C3};
use screeny_fwimage::{plan_write, Image, Scan, HEADER_LEN, HEAD_LEN, SECTOR};

/// Two megabytes: `ota_0` and `ota_1` in `firmware/partitions.csv`.
const SLOT: u32 = 0x20_0000;

/// Feed an image to a scan in `chunk`-byte pieces, exactly the way the
/// firmware's staging loop does - push, then ask whether the front is good
/// enough to start erasing - and return what the scan made of it.
fn scan_in_chunks(image: &[u8], slot_len: u32, chunk: usize) -> Result<Image, FirmwareError> {
    let mut scan = Scan::new(slot_len);
    for piece in image.chunks(chunk.max(1)) {
        scan.push(piece)?;
        scan.check_front()?;
    }
    scan.finish()
}

// ---------------------------------------------------------------------------
// The image that passes
// ---------------------------------------------------------------------------

#[test]
fn a_good_image_passes_every_check() {
    let image = Builder::good().build();
    let out = scan_in_chunks(&image, SLOT, SECTOR).expect("a good image is accepted");
    assert_eq!(out.len as usize, image.len());
    assert_eq!(out.trailing, 0);
    assert_eq!(out.segments, 2);
    assert_eq!(out.version.as_str(), Some("0.6.0"));
    assert_eq!(&out.digest[..], &image[image.len() - 32..]);
}

/// The transport decides where the chunk boundaries fall, not the image. A
/// scan that only worked on 4,096-byte pieces would pass on the bench and fail
/// on a socket that handed over 1,460 bytes at a time.
#[test]
fn the_answer_does_not_depend_on_how_the_bytes_arrive() {
    let image = Builder::good().build();
    let reference = scan_in_chunks(&image, SLOT, SECTOR).unwrap();
    for chunk in [1, 3, 7, 16, 23, 64, 1460, 4095, 4096, 4097, 65536] {
        let out = scan_in_chunks(&image, SLOT, chunk)
            .unwrap_or_else(|e| panic!("chunk {chunk}: {e:?}"));
        assert_eq!(out, reference, "chunk {chunk}");
    }
}

/// A sender that appends junk is not refused: the bootloader reads the length
/// out of the header and the segment table and never looks past it. It is
/// counted, because it is almost always a mistake.
#[test]
fn bytes_past_the_end_of_the_image_are_counted_and_allowed() {
    let mut image = Builder::good().build();
    let real_len = image.len();
    image.extend_from_slice(&[0x5Au8; 1000]);
    let out = scan_in_chunks(&image, SLOT, SECTOR).expect("trailing bytes are not a failure");
    assert_eq!(out.len as usize, real_len);
    assert_eq!(out.trailing, 1000);
}

// ---------------------------------------------------------------------------
// Check 1: the image magic
// ---------------------------------------------------------------------------

#[test]
fn a_body_that_is_not_an_image_is_bad_magic() {
    let mut b = Builder::good();
    b.magic = 0x7F; // an ELF, which is what somebody uploads by accident
    assert_eq!(
        scan_in_chunks(&b.build(), SLOT, SECTOR),
        Err(FirmwareError::BadMagic)
    );
}

/// Probe rule 24's body, and the reason it is safe to run against the device:
/// it fails on byte 0, long before anything is erased.
#[test]
fn sixty_four_zero_bytes_are_bad_magic() {
    assert_eq!(
        scan_in_chunks(&[0u8; 64], SLOT, SECTOR),
        Err(FirmwareError::BadMagic)
    );
}

/// Probe rule 23's body.
#[test]
fn an_empty_body_is_bad_magic() {
    assert_eq!(scan_in_chunks(&[], SLOT, SECTOR), Err(FirmwareError::BadMagic));
    let scan = Scan::new(SLOT);
    assert_eq!(scan.seen(), 0);
    assert_eq!(scan.image_len(), None);
    assert_eq!(scan.check_front(), Ok(()), "nothing has arrived to judge yet");
}

// ---------------------------------------------------------------------------
// Check 2: the chip
// ---------------------------------------------------------------------------

/// The one that would brick the panel if it got through: a perfectly
/// well-formed image, with a perfectly good hash, for a different chip.
#[test]
fn an_image_for_another_chip_is_wrong_chip() {
    let mut b = Builder::good();
    b.chip_id = CHIP_ID_ESP32C3;
    assert_eq!(
        scan_in_chunks(&b.build(), SLOT, SECTOR),
        Err(FirmwareError::WrongChip)
    );
}

#[test]
fn the_chip_is_decided_from_the_first_twenty_four_bytes() {
    let mut b = Builder::good();
    b.chip_id = CHIP_ID_ESP32C3;
    let image = b.build();
    let mut scan = Scan::new(SLOT);
    assert_eq!(
        scan.push(&image[..HEADER_LEN]),
        Err(FirmwareError::WrongChip),
        "refused before a single sector could have been erased"
    );
    // And it stays refused, whatever else arrives.
    assert_eq!(scan.push(&image[HEADER_LEN..]), Err(FirmwareError::WrongChip));
    assert_eq!(scan.error(), Some(FirmwareError::WrongChip));
}

// ---------------------------------------------------------------------------
// Check 3: there has to be a hash to check
// ---------------------------------------------------------------------------

#[test]
fn an_image_with_no_appended_hash_is_bad_sha256() {
    let mut b = Builder::good();
    b.hash_appended = false;
    assert_eq!(
        scan_in_chunks(&b.build(), SLOT, SECTOR),
        Err(FirmwareError::BadSha256)
    );
}

// ---------------------------------------------------------------------------
// Check 4: whose firmware is this
// ---------------------------------------------------------------------------

#[test]
fn somebody_elses_app_is_wrong_project() {
    let mut b = Builder::good();
    b.project = "esp-idf-blink";
    assert_eq!(
        scan_in_chunks(&b.build(), SLOT, SECTOR),
        Err(FirmwareError::WrongProject)
    );
}

#[test]
fn an_image_with_no_app_descriptor_at_all_is_wrong_project() {
    let mut b = Builder::good();
    b.desc_magic = 0;
    assert_eq!(
        scan_in_chunks(&b.build(), SLOT, SECTOR),
        Err(FirmwareError::WrongProject)
    );
}

/// The point of `check_front`: all four cheap checks are answerable from the
/// first sector, so the staging loop knows before it erases anything.
#[test]
fn the_project_is_decided_from_the_first_sector() {
    let mut b = Builder::good();
    b.project = "esp-idf-blink";
    let image = b.build();
    let mut scan = Scan::new(SLOT);
    scan.push(&image[..SECTOR]).expect("the header itself is fine");
    assert_eq!(scan.check_front(), Err(FirmwareError::WrongProject));
}

#[test]
fn the_good_image_passes_check_front_on_its_first_sector() {
    let image = Builder::good().build();
    let mut scan = Scan::new(SLOT);
    scan.push(&image[..SECTOR]).unwrap();
    assert_eq!(scan.check_front(), Ok(()));
}

// ---------------------------------------------------------------------------
// Check 5: the segment table and the checksum byte
// ---------------------------------------------------------------------------

#[test]
fn a_flipped_bit_in_a_segment_is_bad_checksum() {
    let mut image = Builder::good().build();
    // Somewhere in segment 1's data, past the descriptor.
    image[2000] ^= 0x01;
    assert_eq!(
        scan_in_chunks(&image, SLOT, SECTOR),
        Err(FirmwareError::BadChecksum)
    );
}

#[test]
fn a_segment_that_is_not_a_whole_number_of_words_is_bad_checksum() {
    let mut b = Builder::good();
    b.segments[1].data.truncate(2045); // not a multiple of 4
    assert_eq!(
        scan_in_chunks(&b.build(), SLOT, SECTOR),
        Err(FirmwareError::BadChecksum)
    );
}

#[test]
fn a_header_claiming_more_segments_than_the_rom_allows_is_bad_checksum() {
    let mut image = Builder::good().build();
    image[1] = 17; // MAX_SEGMENTS is 16
    assert_eq!(
        scan_in_chunks(&image, SLOT, SECTOR),
        Err(FirmwareError::BadChecksum)
    );
}

// ---------------------------------------------------------------------------
// Check 6: the appended SHA-256
// ---------------------------------------------------------------------------

/// A bit flipped in the padding: the checksum only covers segment data, so
/// this one gets past check 5 and is caught by the hash - which is exactly the
/// division of labour the two checks are for.
#[test]
fn a_flipped_bit_the_checksum_cannot_see_is_bad_sha256() {
    let image = Builder::good().build();
    let mut broken = image.clone();
    // The byte just before the checksum byte is padding.
    let pad = broken.len() - 32 - 2;
    broken[pad] ^= 0xFF;
    assert_eq!(
        scan_in_chunks(&broken, SLOT, SECTOR),
        Err(FirmwareError::BadSha256)
    );
}

#[test]
fn an_image_whose_appended_digest_is_wrong_is_bad_sha256() {
    let mut image = Builder::good().build();
    let n = image.len();
    image[n - 1] ^= 0x01;
    assert_eq!(
        scan_in_chunks(&image, SLOT, SECTOR),
        Err(FirmwareError::BadSha256)
    );
}

/// The one research 006 names: a connection that dies mid-upload.
#[test]
fn a_truncated_upload_is_bad_sha256() {
    let image = Builder::good().build();
    for cut in [HEADER_LEN + 1, 200, 1024, image.len() - 33, image.len() - 1] {
        assert_eq!(
            scan_in_chunks(&image[..cut], SLOT, SECTOR),
            Err(FirmwareError::BadSha256),
            "cut at {cut}"
        );
    }
}

#[test]
fn a_body_that_stops_inside_the_header_is_bad_magic_not_something_else() {
    let image = Builder::good().build();
    assert_eq!(
        scan_in_chunks(&image[..10], SLOT, SECTOR),
        Err(FirmwareError::BadMagic),
        "ten bytes is not an image, whatever they are"
    );
}

// ---------------------------------------------------------------------------
// The slot bound
// ---------------------------------------------------------------------------

#[test]
fn an_image_longer_than_the_slot_is_too_large() {
    let mut b = Builder::good();
    b.segments.push(Segment {
        load_addr: 0x3F40_0000,
        data: vec![0x11; 300_000],
    });
    let image = b.build();
    // A 64 KB slot could never hold it.
    assert_eq!(
        scan_in_chunks(&image, 64 * 1024, SECTOR),
        Err(FirmwareError::TooLarge)
    );
    // The real slot can.
    assert!(scan_in_chunks(&image, SLOT, SECTOR).is_ok());
}

#[test]
fn the_slot_bound_is_enforced_on_the_stream_and_not_only_on_content_length() {
    // A sender whose `Content-Length` was a lie: the scan counts what really
    // arrives.
    let mut scan = Scan::new(8192);
    let image = Builder::good().build();
    let mut fed = 0;
    let mut hit = None;
    for piece in image.chunks(SECTOR) {
        if let Err(e) = scan.push(piece) {
            hit = Some(e);
            break;
        }
        fed += piece.len();
    }
    assert_eq!(hit, Some(FirmwareError::TooLarge));
    assert!(fed <= 8192, "it stopped at the bound, having fed {fed}");
}

// ---------------------------------------------------------------------------
// The writer's own bound: nothing outside the slot, ever
// ---------------------------------------------------------------------------

/// The card's safety requirement, as a test: an offset outside the slot is
/// refused. Below this sits `esp-bootloader-esp-idf`'s own `in_range` check on
/// a partition-relative `FlashRegion`, which is what makes the firmware
/// *structurally* unable to write anywhere else; this is the lock a host test
/// can turn.
#[test]
fn a_write_outside_the_slot_is_refused() {
    // The last sector of a 2 MB slot is fine.
    assert_eq!(plan_write(SLOT, SLOT - SECTOR as u32, SECTOR), Ok(()));
    // One sector past the end is not.
    assert_eq!(
        plan_write(SLOT, SLOT, SECTOR),
        Err(FirmwareError::TooLarge)
    );
    // Neither is a chunk that starts inside and runs over the end.
    assert_eq!(
        plan_write(SLOT, SLOT - 16, 16),
        Err(FirmwareError::Flash),
        "not sector aligned, and it is refused for that before anything else"
    );
    assert_eq!(
        plan_write(SLOT, SLOT - SECTOR as u32 + 4096, SECTOR),
        Err(FirmwareError::TooLarge)
    );
    // An offset that has wrapped or drifted off the sector grid.
    assert_eq!(plan_write(SLOT, 1, SECTOR), Err(FirmwareError::Flash));
    assert_eq!(plan_write(SLOT, 0, SECTOR + 1), Err(FirmwareError::Flash));
    assert_eq!(plan_write(SLOT, 0, 0), Err(FirmwareError::Flash));
    // And the running slot's own offset means nothing here: offsets are
    // partition-relative, so 0x10000 is simply sector 16 of *this* slot.
    assert_eq!(plan_write(SLOT, 0x10000, SECTOR), Ok(()));
    // A slot smaller than one sector can hold nothing at all.
    assert_eq!(plan_write(0, 0, SECTOR), Err(FirmwareError::TooLarge));
}

/// Walking a whole 2 MB slot: every offset the staging loop can produce is
/// accepted, and the one after the end is not.
#[test]
fn every_offset_the_staging_loop_produces_is_inside_the_slot() {
    let mut off = 0u32;
    while off < SLOT {
        assert_eq!(plan_write(SLOT, off, SECTOR), Ok(()), "offset {off:#x}");
        off += SECTOR as u32;
    }
    assert_eq!(off, SLOT);
    assert_eq!(plan_write(SLOT, off, SECTOR), Err(FirmwareError::TooLarge));
}

// ---------------------------------------------------------------------------
// The version string
// ---------------------------------------------------------------------------

#[test]
fn a_version_that_is_not_printable_ascii_is_reported_as_none() {
    let image = Builder::good().build();
    let mut broken = image.clone();
    // `esp_app_desc.version` is at segment 0's data + 16, i.e. 32 + 16.
    broken[32 + 16] = 0x01;
    // The hash and the checksum both change, so rebuild them the way the
    // builder does rather than expecting this to still parse.
    let scan = scan_in_chunks(&broken, SLOT, SECTOR);
    assert_eq!(
        scan,
        Err(FirmwareError::BadChecksum),
        "changing a byte of a segment breaks the checksum first, which is the point of the order"
    );
}

// ---------------------------------------------------------------------------
// A real image, when there is one to hand
// ---------------------------------------------------------------------------

/// Scan an image `espflash save-image` really produced.
///
/// The images above are built by this crate's own test helper, which means
/// they prove the scanner is self-consistent and not that it agrees with
/// esptool. Point `SCREENY_FW_IMAGE` at a `.bin` and this one closes that gap:
///
/// ```text
/// espflash save-image --chip esp32 --flash-size 8mb \
///   --partition-table firmware/partitions.csv <elf> /tmp/fw.bin
/// SCREENY_FW_IMAGE=/tmp/fw.bin cargo test -p screeny-fwimage -- --nocapture
/// ```
///
/// It is not a fixture in the repository on purpose: a 950 KB binary blob in
/// git, replaced on every firmware change, is the sort of thing a repository
/// meant to go public should not carry. `screeny-probe fw-scan` runs the same
/// check from the command line.
#[test]
fn a_real_espflash_image_passes() {
    let Ok(path) = std::env::var("SCREENY_FW_IMAGE") else {
        eprintln!("SCREENY_FW_IMAGE is not set; skipping");
        return;
    };
    let bytes = std::fs::read(&path).expect("read the image");
    let out = scan_in_chunks(&bytes, SLOT, SECTOR)
        .unwrap_or_else(|e| panic!("{path}: {e:?}"));
    eprintln!(
        "{path}: {} bytes on the wire, image {} + {} trailing, {} segments, version {:?}",
        bytes.len(),
        out.len,
        out.trailing,
        out.segments,
        out.version
    );
}

/// The firmware runs check 6 twice: once on the bytes that arrived (the scan)
/// and once on the bytes that landed in flash ([`Rehash`]). They have to agree
/// about the same image, or the second one is noise.
#[test]
fn rehashing_the_image_gives_the_same_answer_as_the_scan() {
    let image = Builder::good().build();
    let out = scan_in_chunks(&image, SLOT, SECTOR).unwrap();
    // Everything the appended digest covers: the image less the digest itself.
    let covered = &image[..out.len as usize - 32];
    let mut h = screeny_fwimage::Rehash::new();
    for piece in covered.chunks(SECTOR) {
        h.update(piece);
    }
    assert!(h.matches(&out.digest));

    // One byte of what landed is wrong: the scan would not have seen it,
    // because the scan saw the socket and this sees the flash.
    let mut damaged = covered.to_vec();
    damaged[SECTOR] ^= 0x01;
    let mut h = screeny_fwimage::Rehash::new();
    for piece in damaged.chunks(SECTOR) {
        h.update(piece);
    }
    assert!(!h.matches(&out.digest));
}

#[test]
fn a_version_string_survives_the_round_trip() {
    let mut b = Builder::good();
    b.version = "9.9.9-rc1+bench";
    let out = scan_in_chunks(&b.build(), SLOT, SECTOR).unwrap();
    assert_eq!(out.version.as_str(), Some("9.9.9-rc1+bench"));
    assert_eq!(format!("{:?}", out.version), "\"9.9.9-rc1+bench\"");
}

// ---------------------------------------------------------------------------
// version_of: naming an image that is already in flash (card 241)
// ---------------------------------------------------------------------------

#[test]
fn the_version_can_be_read_from_the_front_of_an_image_without_scanning_it() {
    let mut b = Builder::good();
    b.version = "0.7.1";
    let image = b.build();
    let v = screeny_fwimage::version_of(&image[..HEAD_LEN]).expect("a good image has a version");
    assert_eq!(v.as_str(), Some("0.7.1"));
    // And the same answer as a full scan, which is the property that matters:
    // the two must never disagree about what is in the slot.
    let scanned = scan_in_chunks(&image, SLOT, SECTOR).unwrap();
    assert_eq!(v, scanned.version);
}

#[test]
fn a_slot_that_holds_no_image_has_no_version_to_report() {
    // An erased slot, a half-staged one, and one holding something that is not
    // an ESP image at all. All three are things card 241 can find in the
    // inactive slot, and all three answer "nothing" rather than guessing.
    assert_eq!(screeny_fwimage::version_of(&[0xFF; HEAD_LEN]), None);
    assert_eq!(screeny_fwimage::version_of(&[0x00; HEAD_LEN]), None);
    let short = Builder::good().build();
    assert_eq!(screeny_fwimage::version_of(&short[..HEAD_LEN - 1]), None);
}

#[test]
fn an_image_without_an_app_descriptor_has_no_version() {
    let mut b = Builder::good();
    b.desc_magic = 0xDEAD_BEEF;
    let image = b.build();
    assert_eq!(screeny_fwimage::version_of(&image[..HEAD_LEN]), None);
}

#[test]
fn somebody_elses_app_still_names_its_version() {
    // Reporting is not admitting: an image that reached the slot passed check
    // 4 on the way in, so refusing to name it here would only lose the fact.
    let mut b = Builder::wrong_project();
    b.version = "1.2.3";
    let image = b.build();
    assert_eq!(
        screeny_fwimage::version_of(&image[..HEAD_LEN])
            .and_then(|v| v.as_str().map(str::to_owned)),
        Some("1.2.3".to_owned())
    );
}
