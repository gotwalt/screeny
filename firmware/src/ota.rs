//! Staging a firmware image into the inactive slot (card 240).
//!
//! `POST /api/v1/firmware` hands the bytes to [`Upload`] as they come off the
//! socket, and they go straight into the app slot this image is **not**
//! running from. Nothing here switches the boot slot: `otadata` is not written
//! by this module at all, and a device that loses power half way through an
//! upload boots exactly what it booted before (research 006 section 5's table,
//! rows 2 to 4). Activate, confirm and revert are card 241.
//!
//! ## The four rules this module exists to keep
//!
//! 1. **The image is never held.** One 4,096-byte buffer, on the heap, for the
//!    life of the upload - not in a future, where it would be `.bss` twice
//!    over (one per HTTP worker) and `.bss` is core 0's stack. Research 006
//!    section 5 allows exactly this: "put the per-chunk work in a synchronous
//!    `fn stage_chunk(...)` that the async HTTP handler calls, **or allocate
//!    from the heap for the life of the update**". Both, here: the buffer is
//!    heap, and the erase-and-write is [`stage_sector`], which is synchronous
//!    and `#[inline(never)]`, so esp-storage's whole call chain is an ordinary
//!    stack frame that is gone before the next `await`.
//!
//! 2. **Only the inactive slot, ever.** The target is a
//!    [`crate::store::InactiveSlot`], which only [`crate::store::read_partitions`]
//!    can make and only after comparing the entry's offset with the MMU's
//!    "which partition am I running from". Every write goes through that
//!    entry's `FlashRegion`, which is partition-relative and bounds-checked by
//!    `esp-bootloader-esp-idf` itself, and through
//!    [`screeny_fwimage::plan_write`], which a host test drives. The running
//!    slot, the bootloader, the partition table and `screeny` are unreachable
//!    from here - not by convention, by type.
//!
//! 3. **Only core 0 touches flash, and it parks core 1 while it does.** This
//!    module does not build a `FlashStorage`: `FlashStorage::new` panics if it
//!    is called twice, and the one on the device is the settings store's,
//!    built with `multicore_auto_park()` (research 006 section 4, rule 1). So
//!    a sector is staged under [`crate::store::STORE`] - the same lock a
//!    `SET_BRIGHTNESS` commit takes - and the lock is taken **per sector**,
//!    not for the upload. A settings write during an upload then waits ~58 ms
//!    rather than ~15 s, and esp-storage's park granularity is a sector
//!    anyway.
//!
//! 4. **One upload at a time.** [`CLAIM`] is the whole mechanism: a second
//!    `POST` gets [`FirmwareError::Busy`] and touches nothing. It is released
//!    by [`Upload`]'s `Drop`, so every way out - a good image, a refused one,
//!    a stalled uploader, a socket that vanished - gives the panel back and
//!    frees the buffer.
//!
//! ## What the panel does
//!
//! `docs/design/device-web.md` decision 7: the frame path is the product and
//! nothing may take the panel from a sender **except a firmware update**.
//! While [`updating`] is true `crate::net::frames_task` draws
//! `screeny_provision::Screen::Updating` and this module forces dither off,
//! restoring whatever it was on the way out. With dither off the display task
//! sleeps and the HUB75 DMA loops on the buffer it has, so the ~50 ms core-1
//! stall around each sector erase costs nothing at all - which is research 006
//! section 4's recommendation, and the reason it is worth doing.
//!
//! A sender streaming at 30 fps is not interrupted and is not told anything:
//! its datagrams are still drained, decoded, counted and answered, and its
//! hold on the source is untouched. What changes for the length of the upload
//! is only which frame reaches the panel. That is exactly the overlay rule
//! card 223 built for the portal screen, and when the upload ends the next
//! decoded frame publishes.
//!
//! ## What it measures
//!
//! Research 006 section 4's open risk is that core 0 has interrupts masked for
//! the whole of each ROM erase and esp-radio does not like that. This module
//! is the instrument: per-sector erase and write times (min / mean / max), and
//! what the radio did over the same window - frames accepted, frames the
//! stream lost, link-down edges, RSSI either side. **One log line per upload**,
//! never per sector: 183 sectors of `info!` through a blocking UART at 230,400
//! baud would be seconds of core 0 spent describing the thing it is measuring.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_storage::nor_flash::NorFlash as _;
use esp_bootloader_esp_idf::ota::{Ota, OtaImageState};
use esp_bootloader_esp_idf::partitions::{AppPartitionSubType, FlashStorage, PartitionEntry};
use esp_hal::rtc_cntl::{Rtc, RwdtStage};
use log::{info, warn};
use screeny_device_api::reply::UpdateRecord;
use screeny_device_api::{FirmwareError, FwSlot, FwState, RevertReason, UpdateOutcome};
use screeny_fwimage::{plan_write, Rehash, Scan, HEAD_LEN, SECTOR};
use screeny_otastate::{classify, decide, Boot, Health, TrialAction};

use crate::store::{self, InactiveSlot};

// ---------------------------------------------------------------------------
// What the rest of the firmware sees
// ---------------------------------------------------------------------------

/// Whether an upload owns the flash and the panel right now.
static CLAIM: AtomicBool = AtomicBool::new(false);

/// How far through, 0..=100, or [`NO_PERCENT`] when the length is unknown.
static PERCENT: AtomicU8 = AtomicU8::new(NO_PERCENT);
const NO_PERCENT: u8 = u8::MAX;

/// Uploads started since boot, and how many of them ended with an accepted
/// image. Both are read by nothing yet; they are here because the difference
/// between "no upload has ever been tried" and "four have been tried and all
/// four failed" is the first thing anybody asks, and an atomic costs 4 bytes.
pub static STARTED: AtomicU32 = AtomicU32::new(0);
/// Uploads that passed every check.
pub static ACCEPTED: AtomicU32 = AtomicU32::new(0);

/// Is an upload in flight? Read by the frame task, once per tick.
#[must_use]
pub fn updating() -> bool {
    CLAIM.load(Ordering::Relaxed)
}

/// What the panel should be showing on account of an update, if anything.
///
/// One question for the frame task instead of three atomics, because the two
/// screens are mutually exclusive and the order between them matters: an
/// upload that has been accepted and is waiting to be activated is
/// [`Panel::Installing`] even though the claim is already back.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// An upload is streaming into the inactive slot.
    Uploading(Option<u8>),
    /// The image is in and the device is about to restart into it.
    Installing,
}

/// The screen an update wants, if it wants one.
#[must_use]
pub fn panel() -> Option<Panel> {
    if ACTIVATING.load(Ordering::Relaxed) {
        Some(Panel::Installing)
    } else if updating() {
        Some(Panel::Uploading(percent()))
    } else {
        None
    }
}

/// How far through the upload is, for the panel.
#[must_use]
pub fn percent() -> Option<u8> {
    match PERCENT.load(Ordering::Relaxed) {
        NO_PERCENT => None,
        p => Some(p),
    }
}

// ---------------------------------------------------------------------------
// The buffer
// ---------------------------------------------------------------------------

/// One 4 KB sector, **four-byte aligned**.
///
/// The alignment is not tidiness. `esp-storage`'s `write_nor` copies a
/// misaligned buffer through *another* 4,096-byte buffer on the caller's stack
/// (its own documentation, "Buffer alignment and stack usage"), which on this
/// chip is core 0's stack inside an HTTP handler - the one place card 227's
/// lesson says not to put 4 KB. Aligned, that copy never happens.
#[repr(C, align(4))]
struct Sector([u8; SECTOR]);

/// Ask the heap for one, or `None` when there is not room.
///
/// `Box::new(Sector([0; SECTOR]))` would build the 4 KB value as a temporary
/// on the stack and then move it, which is the transient this whole design is
/// avoiding; `alloc_zeroed` writes straight into the allocation.
fn alloc_sector() -> Option<Box<Sector>> {
    let layout = core::alloc::Layout::new::<Sector>();
    // SAFETY: the layout is non-zero-sized, so `alloc_zeroed` returns either
    // null or a block of exactly that size and alignment; `Sector` is a plain
    // byte array, for which all-zero is a valid value; and the pointer is
    // handed straight to `Box`, which owns it from here and frees it with the
    // same layout.
    unsafe {
        let p = alloc::alloc::alloc_zeroed(layout).cast::<Sector>();
        if p.is_null() {
            None
        } else {
            Some(Box::from_raw(p))
        }
    }
}

// ---------------------------------------------------------------------------
// Per-sector timing, and what the radio did meanwhile
// ---------------------------------------------------------------------------

/// Erase and write times, in microseconds, accumulated over one upload.
#[derive(Clone, Copy)]
struct Timing {
    sectors: u32,
    erase_total: u64,
    erase_min: u32,
    erase_max: u32,
    write_total: u64,
    write_min: u32,
    write_max: u32,
    /// The longest a single sector took, erase plus write plus the lock.
    slowest_sector: u32,
}

impl Timing {
    const fn new() -> Self {
        Timing {
            sectors: 0,
            erase_total: 0,
            erase_min: u32::MAX,
            erase_max: 0,
            write_total: 0,
            write_min: u32::MAX,
            write_max: 0,
            slowest_sector: 0,
        }
    }

    fn note(&mut self, erase_us: u32, write_us: u32, sector_us: u32) {
        self.sectors += 1;
        self.erase_total += u64::from(erase_us);
        self.erase_min = self.erase_min.min(erase_us);
        self.erase_max = self.erase_max.max(erase_us);
        self.write_total += u64::from(write_us);
        self.write_min = self.write_min.min(write_us);
        self.write_max = self.write_max.max(write_us);
        self.slowest_sector = self.slowest_sector.max(sector_us);
    }

    fn mean(total: u64, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            (total / u64::from(n)) as u32
        }
    }

    /// The number research 006 section 4 actually wants: how much of the
    /// upload core 0 spent with interrupts masked inside a ROM call.
    fn busy_ms(&self) -> u32 {
        ((self.erase_total + self.write_total) / 1000) as u32
    }
}

/// What the stream and the radio were doing when the upload started, so the
/// end of it can report a difference rather than a level.
#[derive(Clone, Copy)]
struct RadioMark {
    frames_rx: u32,
    frames_lost: u32,
    link_downs: u32,
    rssi_dbm: i8,
    at: Instant,
}

impl RadioMark {
    /// Read without taking the `CORE` lock across anything: the telemetry call
    /// is three field reads and the guard is dropped before it returns.
    async fn now() -> Self {
        let (frames_rx, frames_lost) = {
            let mut guard = crate::net::CORE.lock().await;
            match guard.as_mut() {
                Some(core) => {
                    let t = core.telemetry(Instant::now().as_micros());
                    (
                        t.frames_rx,
                        t.frames_dropped_stale
                            + t.frames_dropped_superseded
                            + t.frames_dropped_decode
                            + t.frames_rejected,
                    )
                }
                None => (0, 0),
            }
        };
        RadioMark {
            frames_rx,
            frames_lost,
            link_downs: crate::provision::LINK_DOWNS.load(Ordering::Relaxed),
            rssi_dbm: crate::RSSI_DBM.load(Ordering::Relaxed),
            at: Instant::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// One sector: the only thing in this firmware that erases an app partition
// ---------------------------------------------------------------------------

/// Erase one sector of the staged slot and write `data` into it.
///
/// **Synchronous and `#[inline(never)]` on purpose.** Everything esp-storage
/// puts on the stack below here - the park, the critical section, the ROM
/// wrapper's own frames - lives in a real stack frame that is gone before the
/// caller's next `await`, rather than in an HTTP worker's future. It is the
/// same discipline `store::read_partitions` documents for the 3 KB partition
/// table, for the same reason.
///
/// `data` is at most one sector and is padded up to a whole number of words by
/// the caller, because `NorFlashRegion::WRITE_SIZE` is 4.
#[inline(never)]
fn stage_sector(
    entry: PartitionEntry,
    flash: &mut esp_bootloader_esp_idf::partitions::FlashStorage<'static>,
    offset: u32,
    data: &[u8],
    timing: &mut Timing,
) -> Result<(), FirmwareError> {
    let t0 = Instant::now();
    let mut region = entry.as_flash_region(flash);

    // Erase-as-you-go, exactly one sector. `FlashRegion::erase` of a
    // sector-aligned 4 KB range takes esp-storage's *sector* path and never
    // its 64 KB block path, so the longest core-1 stall stays one sector erase
    // (research 006 section 5). Erasing the whole 2 MB slot up front would be
    // ~25 s of stalls before a byte arrived, and the trailing bytes it would
    // clear are never read: the bootloader takes the image length from the
    // header and the segment table.
    let e0 = Instant::now();
    region
        .erase(offset, offset + SECTOR as u32)
        .map_err(|e| {
            warn!("ota: erase at {:#x} failed: {:?}", offset, e);
            FirmwareError::Flash
        })?;
    let erase_us = e0.elapsed().as_micros() as u32;

    let w0 = Instant::now();
    let mut nor = region.as_nor_flash().map_err(|e| {
        warn!("ota: the staged slot has no NorFlash view: {:?}", e);
        FirmwareError::Flash
    })?;
    // `NorFlashRegion::write`, **not** `FlashRegion::write`: the latter always
    // puts a 4,096-byte sector buffer on the caller's stack and does a full
    // read-modify-erase-write, which is right for a 32-byte `otadata` entry
    // and wrong for bulk staging (research 006 section 5).
    nor.write(offset, data).map_err(|e| {
        warn!("ota: write at {:#x} failed: {:?}", offset, e);
        FirmwareError::Flash
    })?;
    let write_us = w0.elapsed().as_micros() as u32;

    timing.note(erase_us, write_us, t0.elapsed().as_micros() as u32);
    Ok(())
}

/// Read part of the staged slot back into `buf`.
///
/// The read side of check 6: what is verified is what *landed*, not what
/// arrived. See [`Upload::verify_flash`].
///
/// **`NorFlashRegion::read`, not `FlashRegion::read`**, and that is not a
/// detail. `FlashRegion::read` goes to `FlashAccess::flash_read`, which is
/// `esp_storage::FlashStorage::read` - and that opens with
/// `FlashSectorBuffer::uninit()`, a **4,096-byte buffer on the caller's
/// stack** (`esp-storage-0.10.0/src/storage.rs` line 25; it is the 4,160-byte
/// frame in this firmware's disassembly). Inside an HTTP handler that is
/// exactly what card 227's lesson forbids. `read_nor` on a word-aligned
/// destination reads straight into it (`nor_flash.rs` line 72), which is what
/// [`Sector`]'s alignment is for - the same reasoning as
/// `NorFlashRegion::write` on the way in.
#[inline(never)]
fn read_back(
    entry: PartitionEntry,
    flash: &mut esp_bootloader_esp_idf::partitions::FlashStorage<'static>,
    offset: u32,
    buf: &mut [u8],
) -> Result<(), FirmwareError> {
    use embedded_storage::nor_flash::ReadNorFlash as _;
    let mut region = entry.as_flash_region(flash);
    let mut nor = region.as_nor_flash().map_err(|e| {
        warn!("ota: the staged slot has no NorFlash view: {:?}", e);
        FirmwareError::Flash
    })?;
    nor.read(offset, buf).map_err(|e| {
        warn!("ota: read-back at {:#x} failed: {:?}", offset, e);
        FirmwareError::Flash
    })
}

// ---------------------------------------------------------------------------
// The upload
// ---------------------------------------------------------------------------

/// One upload in flight: the claim, the buffer, the scanner and the numbers.
///
/// Built by [`Upload::start`], which is the only thing that can take
/// [`CLAIM`], and released by `Drop`, which is the only thing that gives it
/// back. There is no other exit, so a `?` in the middle of the handler and a
/// socket that vanished mid-body leave the device in the same state: the panel
/// back to the stream, dither back to what it was, the heap buffer freed, and
/// a partly written inactive slot that nothing will ever boot.
pub struct Upload {
    slot: InactiveSlot,
    buf: Box<Sector>,
    /// How much of `buf` is filled.
    fill: usize,
    /// Where the *next* sector goes.
    offset: u32,
    /// Bytes handed to the flash so far.
    written: u32,
    /// What `Content-Length` said, for the progress bar.
    expected: Option<u32>,
    scan: Scan,
    timing: Timing,
    mark: RadioMark,
    /// Whether check 1-4 have been run on the first sector yet.
    front_checked: bool,
    /// The dither setting to put back.
    dither_was: bool,
}

impl Upload {
    /// Claim the flash and the panel, or say why not.
    ///
    /// `declared` is `Content-Length`. Research 006 section 5: "the build card
    /// should also refuse an upload whose `Content-Length` exceeds the slot
    /// size, **before a single sector is erased**" - which is this, and it is
    /// the one refusal that costs the device nothing at all.
    ///
    /// # Errors
    ///
    /// [`FirmwareError::Busy`] if another upload holds the claim,
    /// [`FirmwareError::TooLarge`] if the declared body cannot fit the slot,
    /// [`FirmwareError::Flash`] if the heap cannot spare 4 KB.
    pub async fn start(slot: InactiveSlot, declared: Option<u32>) -> Result<Self, FirmwareError> {
        if declared.is_some_and(|n| n > slot.len()) {
            // Nothing is claimed and nothing is erased: this one is free.
            return Err(FirmwareError::TooLarge);
        }
        // **Card 241, and this is a safety rule rather than a courtesy.**
        // While the running image is on trial, the inactive slot holds the
        // known-good image the device may have to roll back to; staging over
        // it would throw the escape hatch away and leave a device with one
        // unproven image and nowhere to go (research 006 section 6,
        // mitigation 3). The same goes for the window between an accepted
        // upload and the reset that boots it: the slot is spoken for.
        if boot_class().on_trial() || activating() {
            return Err(FirmwareError::Busy);
        }

        // **Every `await` in this function happens before the claim is taken.**
        // That is not tidiness: `crate::http::serve_on` runs inside a `select`
        // against the soft-AP going up or down, so a handler future *can* be
        // dropped where it suspends. Once `Ok` is returned the claim, the
        // buffer and the dither setting all live inside the value, and dropping
        // the future drops the value and runs `Drop`. Between the
        // `compare_exchange` and the struct literal there is no suspension
        // point at all, so there is no window where a cancelled upload could
        // leave the claim held for ever and every later upload answering
        // `busy`.
        let mark = RadioMark::now().await;

        if CLAIM
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(FirmwareError::Busy);
        }
        // The one failure after the claim, and it puts it back by hand because
        // there is no `Upload` yet to do it.
        let Some(buf) = alloc_sector() else {
            CLAIM.store(false, Ordering::Release);
            warn!("ota: no room on the heap for the staging buffer");
            return Err(FirmwareError::Flash);
        };

        STARTED.fetch_add(1, Ordering::Relaxed);
        PERCENT.store(if declared.is_some() { 0 } else { NO_PERCENT }, Ordering::Relaxed);
        // Dither off for the duration (research 006 section 4): the display
        // task then sleeps between frames and the circular DMA loops, so the
        // core-1 stall around each erase costs nothing. Put back on the way
        // out, whatever it was.
        let dither_was = crate::DITHER_ON.swap(false, Ordering::Relaxed);

        info!(
            "ota: upload started - staging into {:#x} ({} KB slot), content-length {:?}",
            slot.offset(),
            slot.len() / 1024,
            declared
        );

        Ok(Upload {
            slot,
            buf,
            fill: 0,
            offset: 0,
            written: 0,
            expected: declared,
            scan: Scan::new(slot.len()),
            timing: Timing::new(),
            mark,
            front_checked: false,
            dither_was,
        })
    }

    /// Bytes handed to the flash so far, for [`FirmwareReply::written`].
    ///
    /// [`FirmwareReply::written`]: screeny_device_api::reply::FirmwareReply::written
    #[must_use]
    pub fn written(&self) -> u32 {
        self.written
    }

    /// Where the socket should put the next bytes it reads.
    ///
    /// **The upload is read straight into the staging buffer**, so the body
    /// crosses no other buffer on its way from smoltcp to the ROM's program
    /// routine. The alternative - a scratch array in the handler and a copy
    /// into here - would be a second buffer living across an `await`, which
    /// is `.bss`, which is core 0's stack. Never empty: [`Upload::took`]
    /// flushes the moment the sector is full.
    pub fn spare(&mut self) -> &mut [u8] {
        &mut self.buf.0[self.fill..]
    }

    /// Account for `n` bytes the socket has just written into
    /// [`spare`](Self::spare).
    ///
    /// # Errors
    ///
    /// The first of research 006's checks this upload breaks, or
    /// [`FirmwareError::Flash`] if a sector would not erase or write.
    pub async fn took(&mut self, n: usize) -> Result<(), FirmwareError> {
        debug_assert!(n <= SECTOR - self.fill);
        let from = self.fill;
        self.fill += n;

        // Scan first, flash second. That order is the whole safety argument
        // behind the probe rules: checks 1-4 are answerable from the first 112
        // bytes, so a wrong-chip or wrong-project image is refused **before
        // the first erase**.
        self.scan.push(&self.buf.0[from..self.fill])?;
        if self.fill == SECTOR {
            self.flush().await?;
        }
        Ok(())
    }

    /// Erase-and-write whatever is in the buffer, then empty it.
    async fn flush(&mut self) -> Result<(), FirmwareError> {
        if self.fill == 0 {
            return Ok(());
        }
        if !self.front_checked {
            self.scan.check_front()?;
            self.front_checked = true;
        }
        // `WRITE_SIZE` is 4, so a short final chunk is rounded up to a word.
        // The padding is `0xFF`, which is what an erased sector already holds,
        // so the bytes past the image are unchanged by being written.
        let len = self.fill.next_multiple_of(4);
        self.buf.0[self.fill..len].fill(0xFF);

        // Our own bound, above esp-bootloader-esp-idf's: a host test drives
        // this one (`crates/fwimage`, `a_write_outside_the_slot_is_refused`).
        plan_write(self.slot.len(), self.offset, len)?;

        // The store's lock, the store's `FlashStorage`, the store's
        // `multicore_auto_park` - per sector, so nothing else waits on flash
        // for longer than one sector (rule 3 in this module's documentation).
        {
            let mut guard = store::STORE.lock().await;
            let Some(f) = guard.as_mut() else {
                warn!("ota: no flash handle");
                return Err(FirmwareError::Flash);
            };
            let entry = self.slot.entry();
            let flash = f.raw();
            stage_sector(entry, flash, self.offset, &self.buf.0[..len], &mut self.timing)?;
        }

        self.written += self.fill as u32;
        self.offset += SECTOR as u32;
        self.fill = 0;
        if let Some(total) = self.expected.filter(|t| *t > 0) {
            let pct = ((u64::from(self.written) * 100) / u64::from(total)).min(100) as u8;
            PERCENT.store(pct, Ordering::Relaxed);
        }
        Ok(())
    }

    /// The body is over: write the tail, run every remaining check, and say
    /// what the image was.
    ///
    /// # Errors
    ///
    /// Whichever of research 006's checks failed.
    pub async fn finish(mut self) -> Result<Accepted, FirmwareError> {
        self.flush().await?;
        let image = self.scan.finish()?;
        // Check 5, on the bytes that *landed* rather than on the bytes that
        // arrived. See [`Upload::verify_flash`] for why this is done here and
        // not with `PartitionEntry::sha256`.
        self.verify_flash(image.len, &image.digest).await?;
        ACCEPTED.fetch_add(1, Ordering::Relaxed);
        let accepted = Accepted {
            written: self.written,
            image_len: image.len,
            version: image.version,
            segments: image.segments,
        };
        self.report("accepted").await;
        Ok(accepted)
    }

    /// Recompute the SHA-256 of what is in the staged slot and compare it with
    /// the digest the image appended.
    ///
    /// Research 006 section 5 names `PartitionEntry::sha256()` for this, and
    /// this does the same arithmetic over the same bytes - but with the 4 KB
    /// read buffer on the **heap** rather than on the stack. The crate's own
    /// call puts a `[u8; 4096]` in `sha256_flash_contents`
    /// (`partitions.rs` line 751), and the only place we could call it from is
    /// inside an HTTP handler, which is precisely where card 227's lesson says
    /// a 4 KB frame must not go. The buffer that is already allocated does the
    /// job, so the check costs no stack and no extra RAM at all.
    ///
    /// Reading it back is also the stronger half of the check: the scanner
    /// verified the bytes that came off the socket, and this verifies the
    /// bytes that came out of the ROM's program routine.
    async fn verify_flash(
        &mut self,
        image_len: u32,
        expect: &[u8; 32],
    ) -> Result<(), FirmwareError> {
        let mut sha = Rehash::new();
        let mut at = 0u32;
        // The appended digest covers everything before itself.
        let end = image_len.saturating_sub(32);
        while at < end {
            // `read_nor` wants a word-aligned offset and length, and gets one
            // by construction: `at` steps by whole sectors, and `end` is the
            // image length less its 32-byte digest, which for an ESP image is
            // always a multiple of 16.
            let n = (end - at).min(SECTOR as u32) as usize;
            debug_assert!(n.is_multiple_of(4));
            {
                let mut guard = store::STORE.lock().await;
                let Some(f) = guard.as_mut() else {
                    return Err(FirmwareError::Flash);
                };
                let entry = self.slot.entry();
                let flash = f.raw();
                read_back(entry, flash, at, &mut self.buf.0[..n])?;
            }
            sha.update(&self.buf.0[..n]);
            at += n as u32;
        }
        if !sha.matches(expect) {
            warn!("ota: the staged slot does not hash to the image's own digest");
            return Err(FirmwareError::BadSha256);
        }
        Ok(())
    }

    /// The one log line per upload: the per-sector timing and what the radio
    /// did over the same window.
    ///
    /// This is research 006 section 4's open risk, instrumented. Two numbers
    /// answer it: `busy` is how much of the upload core 0 spent with
    /// interrupts masked inside a ROM call, and `frames` is what the stream
    /// managed while that was going on. A radio that cannot survive a 50 ms
    /// mask shows up as frames stopping and `link` going above zero.
    async fn report(&self, how: &str) {
        let now = RadioMark::now().await;
        let elapsed_ms = now.at.duration_since(self.mark.at).as_millis() as u32;
        let t = &self.timing;
        info!(
            "ota: upload {} - {} bytes in {} sectors, {} ms wall, {} ms flash-busy ({}%) | erase us min/mean/max {}/{}/{} | write us min/mean/max {}/{}/{} | slowest sector {} us",
            how,
            self.written,
            t.sectors,
            elapsed_ms,
            t.busy_ms(),
            if elapsed_ms == 0 { 0 } else { t.busy_ms() * 100 / elapsed_ms },
            if t.sectors == 0 { 0 } else { t.erase_min },
            Timing::mean(t.erase_total, t.sectors),
            t.erase_max,
            if t.sectors == 0 { 0 } else { t.write_min },
            Timing::mean(t.write_total, t.sectors),
            t.write_max,
            t.slowest_sector,
        );
        info!(
            "ota: the radio meanwhile - frames +{}, stream lost +{}, link downs +{}, rssi {} -> {} dBm, render max {} us, heap {} of {}",
            now.frames_rx.wrapping_sub(self.mark.frames_rx),
            now.frames_lost.wrapping_sub(self.mark.frames_lost),
            now.link_downs.wrapping_sub(self.mark.link_downs),
            self.mark.rssi_dbm,
            now.rssi_dbm,
            crate::RENDER_US_MAX_WINDOW.load(Ordering::Relaxed),
            esp_alloc::HEAP.stats().current_usage,
            esp_alloc::HEAP.stats().size,
        );
    }

    /// Say why an upload ended badly, with the same two lines, so a bench run
    /// has the timing for a refusal as well as for a success.
    pub async fn failed(self, why: FirmwareError) {
        warn!("ota: upload refused: {:?}", why);
        self.report("refused").await;
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        crate::DITHER_ON.store(self.dither_was, Ordering::Relaxed);
        PERCENT.store(NO_PERCENT, Ordering::Relaxed);
        // The panel is the stream's again from the frame task's next tick,
        // which is at most 20 ms away.
        CLAIM.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Card 241: activate, confirm, revert
// ---------------------------------------------------------------------------
//
// Everything below this line is about `otadata`, which is 64 bytes in two
// sectors at 0xE000 and is the only thing the bootloader reads to decide which
// app slot to run. The three writes this firmware makes to it are
// [`write_selection`] (activate) and two calls of [`write_state`] (confirm and
// revert); there are no others, and each is a synchronous `#[inline(never)]`
// call under the settings store's lock, for the reason the staging path has
// one: `FlashRegion::write` on a 32-byte entry does a read-modify-erase-write
// of the whole 4 KB sector with a `[u8; 4096]` on the caller's stack, and card
// 227's lesson is that such a frame must never appear inside an HTTP handler.
//
// The bootloader's half - `NEW -> PENDING_VERIFY` before it hands over, and
// `PENDING_VERIFY -> ABORTED` on **any** reset before it chooses - is ESP-IDF
// v6.1 with `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` (card 242,
// `firmware/bootloader/README.md`). The card's Log quotes the lines.

/// How long the RTC watchdog gives a trial boot that has stopped answering.
///
/// It exists for one failure and one only: an image that **hangs** - before
/// `main`, or with interrupts off - and therefore never resets, so the
/// bootloader's abort loop is never reached and the app-side deadline never
/// runs. `esp_hal::init` disables every watchdog this chip has
/// (`esp-hal-1.2.2/src/lib.rs` lines 768-778), so without this there is nothing
/// armed on the device at all.
///
/// Above [`screeny_otastate::REVERT_AT_MS`] on purpose, and by a minute: a
/// healthy trial disables it at its confirm and an unhealthy one resets itself
/// at 180 s, so the watchdog should never be what fires. When it does, it is
/// the last resort and the reset it causes is the one the bootloader turns into
/// a rollback. It is armed only on a trial boot and is **never fed**, so a
/// confirmed device carries no watchdog it has to keep alive.
const TRIAL_WDT_S: u64 = 240;

/// How long after the reply the activation waits before touching `otadata`.
///
/// The reply is in smoltcp's send buffer when the handler returns; card 236's
/// `BoundedSocket` then closes and waits up to `CLOSE_ACK_MS` = 1500 ms for the
/// peer to acknowledge it. Two seconds is past that bound, so by the time the
/// first sector is erased the caller has its `{"ok":true,...,"activating":true}`
/// and the connection is shut. The panel says `installing` for the whole of it,
/// which is what the two seconds are really for: an update that appeared to
/// hang for two seconds and then vanished would be indistinguishable from one
/// that crashed.
const ACTIVATE_DELAY_MS: u64 = 2_000;

/// How often the trial asks itself whether it has proved anything.
const TRIAL_TICK: Duration = Duration::from_secs(1);

/// How often a trial that is still waiting says so on the log. Six lines for a
/// whole trial, not one a second.
const TRIAL_SAY_MS: u32 = 30_000;

/// What kind of boot this is, as a [`Boot`] discriminant. [`BOOT_UNKNOWN`]
/// until `read_fw_health` has classified it.
static BOOT: AtomicU8 = AtomicU8::new(BOOT_UNKNOWN);
const BOOT_UNKNOWN: u8 = 0;
const BOOT_SETTLED: u8 = 1;
const BOOT_TRIAL: u8 = 2;
const BOOT_REVERTED_DEADLINE: u8 = 3;
const BOOT_REVERTED_ABORTED: u8 = 4;
const BOOT_REVERTED_REJECTED: u8 = 5;

/// The image on trial has confirmed itself. One-way, and the only thing that
/// stops [`trial_task`] writing `VALID` twice.
static CONFIRMED: AtomicBool = AtomicBool::new(false);

/// An accepted upload is waiting to be activated, or is being activated now.
///
/// It is **not** the same flag as [`CLAIM`] and it outlives it: the claim ends
/// when the handler's `Upload` is dropped, and this covers the window from
/// there to the reset. A second upload in that window would stage over an image
/// the device is about to boot, so it is refused.
static ACTIVATING: AtomicBool = AtomicBool::new(false);

/// The activation the handler asked for, delivered to [`activate_task`].
static ACTIVATE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// The slot the last update was installed into, as an [`FwSlot`] discriminant.
static UPDATE_SLOT: AtomicU8 = AtomicU8::new(SLOT_NONE);
const SLOT_NONE: u8 = 0xff;

/// `esp_app_desc.version` of the image the last update installed, read once at
/// boot.
///
/// A `OnceLock` rather than a mutex because it is written exactly once, in the
/// boot path, before any task exists, and read by an HTTP handler thereafter -
/// which is the same shape (and the same type) as `http::CTX`.
static UPDATE_VERSION: embassy_sync::once_lock::OnceLock<screeny_device_api::text::FwText> =
    embassy_sync::once_lock::OnceLock::new();

/// The RTC watchdog, once it has been armed.
///
/// `esp_hal`'s `Rwdt` is a zero-sized handle to a global register block that
/// only `Rtc` can hand out, so the `Rtc` is kept here rather than reconstructed
/// where it is turned off. `None` on every boot that is not a trial, and on
/// those the peripheral is never even taken.
static WDT: Mutex<CriticalSectionRawMutex, Option<Rtc<'static>>> = Mutex::new(None);

/// What kind of boot this is.
#[must_use]
pub fn boot_class() -> Boot {
    match BOOT.load(Ordering::Relaxed) {
        BOOT_SETTLED => Boot::Settled,
        // `Unproven` is promoted to `PendingVerify` in the boot path, so by the
        // time anybody asks there is only one kind of trial.
        BOOT_TRIAL => Boot::Trial,
        BOOT_REVERTED_DEADLINE => Boot::Reverted(RevertReason::Deadline),
        BOOT_REVERTED_ABORTED => Boot::Reverted(RevertReason::Aborted),
        BOOT_REVERTED_REJECTED => Boot::Reverted(RevertReason::Rejected),
        _ => Boot::Unknown,
    }
}

/// Can this device activate a staged image at all?
///
/// `false` when the boot classification says the device cannot read `otadata` -
/// one half-written entry makes both of `esp-bootloader-esp-idf`'s reads fail -
/// in which case an upload is still *staged* correctly but there is no way to
/// select it, and the honest answer to a caller asking for activation is
/// `unavailable` before the megabyte rather than a promise afterwards.
#[must_use]
pub fn can_activate() -> bool {
    boot_class() != Boot::Unknown
}

/// What became of the last update, for `GET /api/v1/panic` (card 241).
///
/// Nothing here touches flash: every value was read once at boot and put in an
/// atomic. The answer changes exactly once while the device runs - when a trial
/// confirms - which is why it belongs on this route and not on the polled one.
#[must_use]
pub fn update_record() -> Option<UpdateRecord> {
    let slot = match UPDATE_SLOT.load(Ordering::Relaxed) {
        x if x == FwSlot::Ota0 as u8 => FwSlot::Ota0,
        x if x == FwSlot::Ota1 as u8 => FwSlot::Ota1,
        _ => return None,
    };
    let (outcome, reason) = match boot_class() {
        Boot::Trial if CONFIRMED.load(Ordering::Relaxed) => (UpdateOutcome::Confirmed, None),
        Boot::Trial => (UpdateOutcome::Trial, None),
        Boot::Reverted(r) => (UpdateOutcome::Reverted, Some(r)),
        // A settled boot has nothing to report: the update it came from, if
        // there was one, confirmed on an earlier boot and `otadata` has kept
        // no memory of it beyond "this slot is valid", which `fw_slot` and
        // `fw_state` already say.
        Boot::Settled | Boot::Unproven | Boot::Unknown => return None,
    };
    Some(UpdateRecord {
        outcome,
        reason,
        slot,
        version: UPDATE_VERSION.try_get().cloned(),
    })
}

// ---------------------------------------------------------------------------
// The three writes
// ---------------------------------------------------------------------------

/// Write a state onto the entry `otadata` currently selects.
///
/// **The entry it writes is the one with the highest sequence number**, which
/// on a trial boot is the running image's own. That is only true on a trial
/// boot - after a rollback the highest sequence belongs to the image that was
/// rolled back - so every caller of this is on a path that has already checked
/// the boot is a trial. See the card's Log, "the trap in
/// `esp-bootloader-esp-idf`'s own bookkeeping".
#[inline(never)]
fn write_state(
    entry: PartitionEntry,
    flash: &mut FlashStorage<'static>,
    state: OtaImageState,
) -> Result<(), ()> {
    let mut ota = Ota::new(entry.as_flash_region(flash), 2).map_err(|e| {
        warn!("ota: otadata is not usable: {:?}", e);
    })?;
    ota.set_current_ota_state(state).map_err(|e| {
        warn!("ota: could not write otadata state {:?}: {:?}", state, e);
    })
}

/// Point `otadata` at `app` and mark it `NEW`: the activation, both writes.
///
/// The order is 006 section 6's - select, then mark - and the window between
/// the two is interruption 5b in the card's Log: losing power there leaves the
/// staged slot selected with a state of `Undefined`, which the next boot
/// promotes to `PENDING_VERIFY` rather than letting it run unproven. The
/// sequence write lands on the entry the device is **not** booting from
/// (`Ota::set_current_app_partition` writes `current_slot().next()`), so at no
/// point is the entry that is keeping this device alive in flight.
#[inline(never)]
fn write_selection(
    entry: PartitionEntry,
    flash: &mut FlashStorage<'static>,
    app: AppPartitionSubType,
) -> Result<(), ()> {
    let mut ota = Ota::new(entry.as_flash_region(flash), 2).map_err(|e| {
        warn!("ota: otadata is not usable: {:?}", e);
    })?;
    ota.set_current_app_partition(app).map_err(|e| {
        warn!("ota: could not select {:?}: {:?}", app, e);
    })?;
    ota.set_current_ota_state(OtaImageState::New).map_err(|e| {
        warn!("ota: selected {:?} but could not mark it NEW: {:?}", app, e);
    })
}

/// Read the front of a slot and say what version is in it.
///
/// [`HEAD_LEN`] is 112 bytes, which is the image header, the first segment
/// header and `esp_app_desc` up to the end of `version`. The buffer is
/// word-aligned so that `read_nor` reads straight into it and esp-storage's
/// 4 KB sector buffer never appears (the same reasoning as [`read_back`]).
#[inline(never)]
fn read_version(
    entry: PartitionEntry,
    flash: &mut FlashStorage<'static>,
) -> Option<screeny_fwimage::Version> {
    use embedded_storage::nor_flash::ReadNorFlash as _;
    #[repr(C, align(4))]
    struct Head([u8; HEAD_LEN]);
    let mut head = Head([0; HEAD_LEN]);
    let mut region = entry.as_flash_region(flash);
    let mut nor = region.as_nor_flash().ok()?;
    nor.read(0, &mut head.0).ok()?;
    screeny_fwimage::version_of(&head.0)
}

// ---------------------------------------------------------------------------
// The boot classification
// ---------------------------------------------------------------------------

/// Work out what kind of boot this is, say so, and arm whatever it needs.
///
/// Called once, from [`crate::http::read_fw_health`], with the `STORE` lock
/// held and the flash handle borrowed - so everything here that touches flash
/// does it in one pass, under one lock, before any task exists.
///
/// Returns the state to report as `fw_state`, which may differ from what was
/// read: an `Undefined` entry that selects the running slot is promoted to
/// `PENDING_VERIFY` here, and from then on the device really is on trial.
pub fn note_boot(
    booted: FwSlot,
    selected: FwSlot,
    state: FwState,
    parts: &store::Parts,
    flash: &mut FlashStorage<'static>,
) -> FwState {
    let mut class = classify(booted, selected, state);
    let mut state = state;

    // Interruption 5b. One `otadata` write, on a boot that nothing normal
    // produces, and the alternative is letting an image nobody vouched for
    // become permanent because the power went out between two sector writes.
    if class == Boot::Unproven {
        warn!(
            "ota: {:?} is selected but was never marked - an activation was interrupted. Putting it on trial.",
            booted
        );
        if parts
            .otadata
            .is_some_and(|e| write_state(e, flash, OtaImageState::PendingVerify).is_ok())
        {
            state = FwState::PendingVerify;
            class = Boot::Trial;
        }
    }

    // The slot the last update was installed into, and the version in it: the
    // running one for a trial, the *other* one for a revert - which is the
    // image that was rejected and the thing anybody asks about first.
    let (slot, entry) = match class {
        Boot::Trial => (booted, None),
        Boot::Reverted(_) => (
            match booted {
                FwSlot::Ota0 => FwSlot::Ota1,
                FwSlot::Ota1 => FwSlot::Ota0,
                FwSlot::Unknown => FwSlot::Unknown,
            },
            parts.inactive.map(|s| s.entry()),
        ),
        _ => (FwSlot::Unknown, None),
    };
    UPDATE_SLOT.store(slot as u8, Ordering::Relaxed);
    let version = match (class, entry) {
        // A trial is running the image in question, so its version is this
        // build's own string and needs no flash read at all.
        (Boot::Trial, _) => screeny_device_api::text::text(crate::FW_VERSION),
        (Boot::Reverted(_), Some(e)) => read_version(e, flash)
            .and_then(|v| v.as_str().and_then(screeny_device_api::text::text)),
        _ => None,
    };
    if let Some(v) = version {
        let _ = UPDATE_VERSION.init(v);
    }

    BOOT.store(
        match class {
            Boot::Settled => BOOT_SETTLED,
            Boot::Trial | Boot::Unproven => BOOT_TRIAL,
            Boot::Reverted(RevertReason::Deadline) => BOOT_REVERTED_DEADLINE,
            Boot::Reverted(RevertReason::Aborted) => BOOT_REVERTED_ABORTED,
            Boot::Reverted(RevertReason::Rejected) => BOOT_REVERTED_REJECTED,
            Boot::Unknown => BOOT_UNKNOWN,
        },
        Ordering::Relaxed,
    );

    // **The one line at boot that says an update was rolled back.** It is a
    // `warn!` because it is the one thing in this log somebody looking for
    // "why is it still on the old version" needs to find.
    match class {
        Boot::Settled => {}
        Boot::Trial | Boot::Unproven => info!(
            "ota: ON TRIAL - this boot is a firmware update's first run from {:?}. It confirms itself once it is healthy (never before {} s) or reverts at {} s.",
            booted,
            screeny_otastate::CONFIRM_NOT_BEFORE_MS / 1000,
            screeny_otastate::REVERT_AT_MS / 1000,
        ),
        Boot::Reverted(reason) => warn!(
            "ota: REVERTED - the last firmware update did not stick. {} was rolled back ({}), and this device is running {:?} again, fw {}.",
            match slot {
                FwSlot::Ota0 => "ota_0",
                FwSlot::Ota1 => "ota_1",
                FwSlot::Unknown => "the other slot",
            },
            match reason {
                RevertReason::Deadline =>
                    "it booted but never became healthy, so it marked itself invalid",
                RevertReason::Aborted =>
                    "it reset before it could confirm itself - a panic, a watchdog or a power cut; GET /api/v1/panic says which",
                RevertReason::Rejected => "the bootloader would not run it at all",
            },
            booted,
            crate::FW_VERSION,
        ),
        Boot::Unknown => warn!(
            "ota: this device cannot say which slot otadata selects, so it will not write to it. A serial flash (tools/fw-run.sh erases otadata) is what clears this."
        ),
    }

    // RTC memory's copy of "a trial is expected" has done its job - the
    // watchdog is either armed or not by now - and it must not survive into a
    // boot that is not one. `otadata` is the record from here on.
    crate::panic::set_ota_trial(class.on_trial());
    state
}

// ---------------------------------------------------------------------------
// The watchdog
// ---------------------------------------------------------------------------

/// Arm the RTC watchdog for [`TRIAL_WDT_S`], before anything else can hang.
///
/// Called from `main` immediately after `esp_hal::init`, and only when RTC
/// memory says the boot that is starting is a trial - which is early enough to
/// cover `esp_hal::init`'s own work, the heap, the store and the panel, i.e.
/// everything a newly written image could wedge in before it could read
/// `otadata` and find out it was on trial.
pub async fn arm_watchdog(mut rtc: Rtc<'static>) {
    rtc.rwdt.set_timeout(
        RwdtStage::Stage0,
        esp_hal::time::Duration::from_secs(TRIAL_WDT_S),
    );
    rtc.rwdt.enable();
    *WDT.lock().await = Some(rtc);
    warn!(
        "ota: trial boot - RTC watchdog armed for {} s (it is never fed; confirming turns it off)",
        TRIAL_WDT_S
    );
}

/// Turn it off again: this boot is not a trial, or the trial has confirmed.
pub async fn disarm_watchdog() {
    if let Some(rtc) = WDT.lock().await.as_mut() {
        rtc.rwdt.disable();
        info!("ota: RTC watchdog disabled");
    }
}

// ---------------------------------------------------------------------------
// Confirm and revert: the image on trial decides its own fate
// ---------------------------------------------------------------------------

/// Everything the health criterion asks about this device, right now.
fn health() -> Health {
    // The `ota-test-unhealthy` bench build (card 241): an image that boots,
    // joins, serves and draws perfectly well and simply never says so. It is
    // the one shape of failure the bootloader cannot see - nothing resets -
    // so the app-side deadline is the only thing that can end its trial, which
    // is precisely what the bench is being asked to demonstrate.
    #[cfg(feature = "ota-test-unhealthy")]
    return Health {
        uptime_ms: crate::now_ms(),
        has_ip: false,
        http_requests: 0,
        swaps: 0,
    };
    #[cfg(not(feature = "ota-test-unhealthy"))]
    Health {
        uptime_ms: crate::now_ms(),
        // WiFi associated *and* DHCP bound: the frame task publishes the
        // station's address here on every 20 ms tick, and it is `None` - so
        // zero - for the whole of a trial join as well as when there is no
        // link at all.
        has_ip: crate::http::IPV4.load(Ordering::Relaxed) != 0,
        http_requests: crate::http::REQUESTS.load(Ordering::Relaxed),
        swaps: crate::SWAPS.load(Ordering::Relaxed),
    }
}

/// The image on trial, proving itself or giving up.
///
/// Spawned by `main` **only** when the boot classification is a trial, which is
/// what makes "it is impossible to confirm from a state that is not a trial"
/// true by construction rather than by a check inside the loop: on every other
/// boot this task does not exist and `otadata` is not written at all.
#[embassy_executor::task]
pub async fn trial_task() {
    let mut said_at: u32 = 0;
    loop {
        Timer::after(TRIAL_TICK).await;
        let h = health();
        match decide(h) {
            TrialAction::Wait => {
                if h.uptime_ms.wrapping_sub(said_at) >= TRIAL_SAY_MS {
                    said_at = h.uptime_ms;
                    info!(
                        "ota: trial at {} s - ip {}, http {}, swaps {} (healthy {})",
                        h.uptime_ms / 1000,
                        h.has_ip,
                        h.http_requests,
                        h.swaps,
                        h.is_healthy(),
                    );
                }
            }
            TrialAction::Confirm => {
                if confirm(&h).await {
                    return;
                }
                // The write failed. Keep trying: the deadline is still ahead,
                // and if it arrives first the revert path takes over - which
                // is the right way round, because an image that cannot write
                // `otadata` has not proved it is healthy.
            }
            TrialAction::Revert => revert(&h).await,
        }
    }
}

/// Mark the running slot `VALID`. Returns whether it stuck.
async fn confirm(h: &Health) -> bool {
    if CONFIRMED.load(Ordering::Relaxed) {
        return true;
    }
    let ok = {
        let mut guard = store::STORE.lock().await;
        match guard.as_mut() {
            Some(f) => {
                let entry = f.parts().otadata;
                let flash = f.raw();
                match entry {
                    Some(e) => write_state(e, flash, OtaImageState::Valid).is_ok(),
                    None => false,
                }
            }
            None => false,
        }
    };
    if !ok {
        warn!("ota: could not confirm this image - will try again on the next tick");
        return false;
    }
    CONFIRMED.store(true, Ordering::Relaxed);
    crate::http::set_fw_state(FwState::Valid);
    crate::panic::set_ota_trial(false);
    disarm_watchdog().await;
    info!(
        "ota: CONFIRMED at {} s - fw {} is now this device's firmware (otadata says valid). ip {}, http requests {}, swaps {}.",
        h.uptime_ms / 1000,
        crate::FW_VERSION,
        h.has_ip,
        h.http_requests,
        h.swaps,
    );
    true
}

/// Mark the running slot `INVALID` and restart, so the bootloader picks the
/// other one.
async fn revert(h: &Health) -> ! {
    warn!(
        "ota: REVERTING at {} s - fw {} never became healthy (ip {}, http requests {}, swaps {}). Marking this slot invalid and restarting into the previous image.",
        h.uptime_ms / 1000,
        crate::FW_VERSION,
        h.has_ip,
        h.http_requests,
        h.swaps,
    );
    {
        let mut guard = store::STORE.lock().await;
        if let Some(f) = guard.as_mut() {
            let entry = f.parts().otadata;
            let flash = f.raw();
            if let Some(e) = entry {
                // A failure here is survivable and is *not* a reason to stay:
                // the entry is still `PENDING_VERIFY`, and the bootloader
                // aborts one of those on the reset that is two lines away.
                let _ = write_state(e, flash, OtaImageState::Invalid);
            }
        }
    }
    crate::panic::set_ota_trial(false);
    // Long enough for the two lines above to leave the blocking UART, and for
    // any sector erase to finish - the same reasoning as the panic path's
    // settle, at a point where there is no hurry at all.
    Timer::after(Duration::from_millis(200)).await;
    esp_hal::system::software_reset()
}

// ---------------------------------------------------------------------------
// Activation: the deferred half of POST /api/v1/firmware
// ---------------------------------------------------------------------------

/// Ask for the staged image to be activated, once the reply is on the wire.
///
/// Called by the HTTP handler **after** it has built its reply and before it
/// returns, so the flash write and the reset happen where card 227's lesson
/// says they must: not in the handler.
pub fn request_activation() {
    ACTIVATING.store(true, Ordering::Relaxed);
    ACTIVATE.signal(());
}

/// The counterpart of [`request_activation`] for a device that has just
/// accepted an image: is one already on its way to being booted?
#[must_use]
pub fn activating() -> bool {
    ACTIVATING.load(Ordering::Relaxed)
}

/// Write `otadata` and restart, once the reply has had time to leave.
///
/// A task and not part of the handler, for two reasons that both matter: the
/// 4 KB sector-buffer frame must not sit under picoserve's response chain, and
/// the reply must be acknowledged before the connection is destroyed by a
/// reboot. See [`ACTIVATE_DELAY_MS`].
#[embassy_executor::task]
pub async fn activate_task() {
    ACTIVATE.wait().await;
    Timer::after(Duration::from_millis(ACTIVATE_DELAY_MS)).await;

    let target = {
        let guard = store::STORE.lock().await;
        guard.as_ref().and_then(|f| f.parts().inactive)
    };
    let Some(slot) = target else {
        warn!("ota: asked to activate with no inactive slot - nothing done");
        ACTIVATING.store(false, Ordering::Relaxed);
        return;
    };
    let Some(app) = app_subtype(&slot) else {
        warn!("ota: the staged slot has a label this build does not know - nothing done");
        ACTIVATING.store(false, Ordering::Relaxed);
        return;
    };

    let ok = {
        let mut guard = store::STORE.lock().await;
        match guard.as_mut() {
            Some(f) => {
                let entry = f.parts().otadata;
                let flash = f.raw();
                match entry {
                    Some(e) => write_selection(e, flash, app).is_ok(),
                    None => false,
                }
            }
            None => false,
        }
    };
    if !ok {
        warn!(
            "ota: the staged image is still in {:#x} but otadata could not be written - this device is unchanged and is still running fw {}",
            slot.offset(),
            crate::FW_VERSION
        );
        ACTIVATING.store(false, Ordering::Relaxed);
        return;
    }

    // RTC memory, so that the next boot can arm the watchdog before it is able
    // to read a single byte of flash.
    crate::panic::set_ota_trial(true);
    info!(
        "ota: ACTIVATED {:#x} - restarting into it on trial. If it does not prove itself within {} s, or resets before it does, the bootloader brings fw {} back.",
        slot.offset(),
        screeny_otastate::REVERT_AT_MS / 1000,
        crate::FW_VERSION
    );
    Timer::after(Duration::from_millis(200)).await;
    esp_hal::system::software_reset()
}

/// The `ota-test-panic` bench build: panic twenty seconds into **every** boot.
///
/// Card 243's `panic-test` fires once per power-on, because its job is to prove
/// the breadcrumb survives a reset. This one has the opposite job - it is an
/// *update* that is broken, and the thing being proved is that the bootloader
/// takes the panel back to the previous image - so it must not quietly become
/// healthy on the second boot. In practice it panics exactly once, because the
/// second boot is the old image; if it ever panics twice, the rollback did not
/// happen and that is the finding.
#[cfg(feature = "ota-test-panic")]
#[embassy_executor::task]
pub async fn panic_test_task() {
    const AFTER_S: u64 = 20;
    warn!(
        "ota-test: BENCH BUILD - panicking on core 0 in {} s, on every boot (card 241)",
        AFTER_S
    );
    Timer::after(Duration::from_secs(AFTER_S)).await;
    panic!("ota-test: deliberate panic during an OTA trial (card 241)");
}

/// The partition subtype for a slot, from its **label**.
///
/// By label and not by `PartitionEntry::partition_type()`, which `unwrap!`s its
/// conversion and would panic on a subtype this crate's enums do not know
/// (research 006 section 3), and not by hard-coded offset, which a
/// re-partitioned device would make a lie. `firmware/partitions.csv` is the
/// source of truth for both the names and the places.
fn app_subtype(slot: &InactiveSlot) -> Option<AppPartitionSubType> {
    match slot.entry().label_as_str() {
        "ota_0" => Some(AppPartitionSubType::Ota0),
        "ota_1" => Some(AppPartitionSubType::Ota1),
        _ => None,
    }
}

/// What a completed, accepted upload turned out to be.
#[derive(Clone, Copy)]
pub struct Accepted {
    /// Bytes written to the slot.
    pub written: u32,
    /// The image's own length, which is what the bootloader would read.
    pub image_len: u32,
    /// `esp_app_desc.version` out of the image.
    pub version: screeny_fwimage::Version,
    /// Segments it declared.
    pub segments: u8,
}
