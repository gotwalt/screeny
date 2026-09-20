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

use embassy_time::Instant;
use embedded_storage::nor_flash::NorFlash as _;
use esp_bootloader_esp_idf::partitions::PartitionEntry;
use log::{info, warn};
use screeny_device_api::FirmwareError;
use screeny_fwimage::{plan_write, Rehash, Scan, SECTOR};

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
