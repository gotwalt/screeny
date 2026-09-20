//! The device half of the settings store: the flash, the clock and the task.
//!
//! [`screeny_settings`] owns the layout - the key set, the encoding, the schema
//! version, the load/save policy and the debounce timer - and it is `no_std`,
//! no-alloc, no-clock and no-I/O so that a plain `cargo test` covers it on the
//! host. What is left here is what only a *device* has: the `screeny` partition,
//! `esp-storage`'s multi-core discipline, an embassy task with a real clock, and
//! the counters a status page will want.
//!
//! ## Three rules this module exists to keep
//!
//! 1. **Only core 0 ever touches flash, and it parks core 1 while it does.**
//!    [`FlashStorage::multicore_auto_park`], never `multicore_ignore`. Core 1
//!    fetches its instructions from the same flash the ROM routine is erasing;
//!    parking it is a hardware clock stall, so the HUB75 DMA keeps scanning the
//!    buffer it already has and only the dither phase freezes. Research 006
//!    section 4 measured the granularity: `esp-storage` parks and unparks around
//!    *each* sector, not around a whole update.
//! 2. **No big buffer lives across an `await`.** An embassy task's future is a
//!    `static`, so anything held across a suspension point is `.bss`, and on this
//!    chip `.bss` comes straight out of core 0's main stack. The 3 KB partition
//!    table is therefore read inside [`read_partitions`], which is deliberately
//!    **not** `async` and `#[inline(never)]`: its buffer is on a real stack frame
//!    that is gone before anything suspends. All that survives is a handful of
//!    32-byte [`PartitionEntry`]s. (Card 200 lost 11 KB of stack to exactly
//!    this.) Since card 243 it is also read **once for the whole boot**, because
//!    a second copy of that buffer sat underneath the deepest chain the boot
//!    path has.
//! 3. **The PSK is never logged.** Nothing in this file formats a [`Psk`], and
//!    `Psk`'s own `Debug` prints only a byte count. The SSID is not printed here
//!    either: the one place the firmware says an SSID out loud is the Wi-Fi
//!    task's "connected" line, and it stays the only one.
//!
//! ## Why there is no command channel
//!
//! The debounced fields (name, brightness, idle mode) are announced with one
//! [`AtomicU8`] bitmask plus one [`Signal`]: 1 byte and ~16 bytes of `.bss`, and
//! - the point - `note_dirty` is infallible and cannot block, which matters
//! because it is called from inside the receiver's synchronous control handler.
//! A `Channel<Cmd, 4>` would have cost ~4 x `size_of::<Cmd>()` and its
//! `try_send` would return `Full` in exactly the situation where losing the
//! notification is unacceptable. The bitmask cannot fill: two changes to the
//! same field coalesce, which is what the debounce wants anyway.
//!
//! The *immediate* writes (`SET_NAME`, `SET_WIFI`) do not go through the task at
//! all. Spec section 6.5's `ERR_STORAGE` can only be honest if the write happens
//! before the reply, so [`crate::net::control_task`] performs them itself,
//! against the same [`STORE`] mutex the task uses, and downgrades the reply to
//! `ERR_STORAGE` when one fails.

use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, Instant};
use embedded_storage::nor_flash::{
    ErrorType, NorFlash as BlockingNorFlash, ReadNorFlash as BlockingReadNorFlash,
};
use esp_bootloader_esp_idf::partitions::{
    self, FlashStorage, PartitionEntry, PARTITION_TABLE_MAX_LEN,
};
use log::{info, warn};
use screeny_settings::{
    Debounce, Field, IdleMode, LoadReport, Name, SchemaState, Scratch, Settings, Store, StoreError,
    Wifi, Write,
};

/// The label of the settings partition in `firmware/partitions.csv`.
///
/// Looked up by **label**, never by type: its subtype is `undefined` (0x06) and
/// `PartitionEntry::partition_type()` `unwrap!`s the conversion, so asking the
/// wrong question would panic on the device (research 006 section 3).
const LABEL: &str = "screeny";

/// The label of the OTA selection partition in `firmware/partitions.csv`.
///
/// By label for the same reason as [`LABEL`], and read in the same pass since
/// card 243: see [`read_partitions`].
const OTADATA_LABEL: &str = "otadata";

// ---------------------------------------------------------------------------
// Counters — what a status page (card 222) reads
// ---------------------------------------------------------------------------

/// Settings actually written to flash since boot.
pub static COMMITS: AtomicU32 = AtomicU32::new(0);
/// Saves that found flash already holding the value and wrote nothing. This is
/// the common case behind a debounced slider.
pub static SKIPS: AtomicU32 = AtomicU32::new(0);
/// Failed writes. A *debounced* commit's reply has already gone out by the time
/// it fails (see the module docs), so this counter is the only record of one.
pub static FAILURES: AtomicU32 = AtomicU32::new(0);
/// 4 KB sectors erased by the store since boot, counted at the `NorFlash` call
/// itself rather than guessed from a duration. This is the number card 063's
/// "a 60 s brightness sweep costs a handful of writes" is really about.
pub static ERASES: AtomicU32 = AtomicU32::new(0);
/// `NorFlash::write` calls the store has made since boot.
pub static PAGE_WRITES: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// The dirty bitmask: the whole "command channel"
// ---------------------------------------------------------------------------

/// The friendly name changed and should be written after the quiet period.
///
/// `SET_NAME` writes immediately (it can answer `ERR_STORAGE`); this bit exists
/// for the boot-time seed and for any future non-control path.
pub const DIRTY_NAME: u8 = 1 << 0;
/// The brightness changed.
pub const DIRTY_BRIGHTNESS: u8 = 1 << 1;
/// The idle mode changed.
pub const DIRTY_IDLE: u8 = 1 << 2;

static DIRTY: AtomicU8 = AtomicU8::new(0);
static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Note that a debounced setting changed. Infallible, non-blocking, and safe to
/// call from the receiver's synchronous control handler.
pub fn note_dirty(bits: u8) {
    DIRTY.fetch_or(bits, Ordering::Relaxed);
    WAKE.signal(());
}

// ---------------------------------------------------------------------------
// Counting the flash operations
// ---------------------------------------------------------------------------

/// A `NorFlash` that counts erases and writes before delegating.
///
/// Wrapping the *blocking* region (rather than timing the outside of a save and
/// guessing) is what lets the bench say "this write cost one sector erase"
/// instead of "this write took 54 ms, so probably".
struct Counted<T>(T);

impl<T: ErrorType> ErrorType for Counted<T> {
    type Error = T::Error;
}

impl<T: BlockingReadNorFlash> BlockingReadNorFlash for Counted<T> {
    const READ_SIZE: usize = T::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl<T: BlockingNorFlash> BlockingNorFlash for Counted<T> {
    const WRITE_SIZE: usize = T::WRITE_SIZE;
    const ERASE_SIZE: usize = T::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let sectors = (to.saturating_sub(from) as usize).div_ceil(T::ERASE_SIZE) as u32;
        ERASES.fetch_add(sectors, Ordering::Relaxed);
        self.0.erase(from, to)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        PAGE_WRITES.fetch_add(1, Ordering::Relaxed);
        self.0.write(offset, bytes)
    }
}

// ---------------------------------------------------------------------------
// The flash handle
// ---------------------------------------------------------------------------

/// Everything the store needs that is not in `screeny-settings`.
///
/// It is small on purpose: the `FlashStorage` handle, the 32-byte partition
/// entry, and one aligned 128-byte [`Scratch`]. Keeping the scratch buffer
/// *here* - in a value behind a `static` mutex - rather than in a task's local
/// is what keeps it out of any future and makes its 128 bytes of `.bss`
/// deliberate.
///
/// The `Store` itself is built per operation. It has to be: a `NorFlashRegion`
/// borrows the `FlashRegion`, which borrows the `FlashStorage`, so a long-lived
/// `Store` would be a self-referential struct. Building one is a `MapConfig`
/// range check and a struct literal - no I/O, no scan - because the map's cache
/// is `Cache::new_uncached()`.
pub struct Flash {
    flash: FlashStorage<'static>,
    entry: PartitionEntry,
    scratch: Scratch,
    /// The rest of what the one partition-table read found (card 243). It
    /// lives here, behind the `STORE` lock, rather than travelling through
    /// `main` as a local: a local would be held across `main`'s first `await`
    /// and therefore be `.bss` twice over, and everything that wants a
    /// partition has to take this lock anyway.
    parts: Parts,
}

/// The one flash handle. `None` until [`init`] has run, and `None` forever if
/// the `screeny` partition is missing - in which case every save reports
/// [`StoreError::Flash`] and the device runs on defaults rather than refusing
/// to work.
pub static STORE: Mutex<CriticalSectionRawMutex, Option<Flash>> = Mutex::new(None);

/// What one save did, with the cost it actually paid.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// What the store decided.
    pub write: Write,
    /// Wall-clock microseconds for the whole call, including the fetch that
    /// decides whether to skip.
    pub us: u32,
    /// 4 KB sector erases inside it. Normally 0; 1 when the map rolled onto the
    /// next page.
    pub erases: u32,
}

macro_rules! timed {
    ($body:expr) => {{
        let t0 = Instant::now();
        let e0 = ERASES.load(Ordering::Relaxed);
        let r = $body;
        let us = t0.elapsed().as_micros() as u32;
        let erases = ERASES.load(Ordering::Relaxed).wrapping_sub(e0);
        match r {
            Ok(write) => {
                match write {
                    Write::Committed => COMMITS.fetch_add(1, Ordering::Relaxed),
                    Write::Skipped => SKIPS.fetch_add(1, Ordering::Relaxed),
                };
                Ok(Timing { write, us, erases })
            }
            Err(e) => {
                FAILURES.fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
        }
    }};
}

/// Open a [`Store`] over the partition, run one call on it, and drop it again.
///
/// `$f` is written as `|store, scratch| store.some_call(scratch, ..)`; the
/// macro exists only because the borrow chain (entry -> region -> nor -> store)
/// has to be re-established for every operation and writing it out five times
/// would be five chances to get the lifetimes subtly different.
macro_rules! with_store {
    ($self:expr, $store:ident, $scratch:ident, $call:expr) => {{
        let Flash {
            flash,
            entry,
            scratch,
            ..
        } = $self;
        let len = entry.len();
        let mut region = entry.as_flash_region(flash);
        let nor = match region.as_nor_flash() {
            Ok(n) => n,
            Err(_) => return Err(StoreError::Flash),
        };
        let mut $store = match Store::new(BlockingAsync::new(Counted(nor)), 0..len) {
            Ok(s) => s,
            // The range came from the partition table, so this is a malformed
            // table rather than a runtime condition; treat it as a dead store.
            Err(_) => return Err(StoreError::Internal),
        };
        let $scratch = scratch.as_mut_slice();
        $call
    }};
}

impl Flash {
    /// The raw flash handle, for the things that read a *different* partition.
    ///
    /// `FlashStorage::new` panics if it is called twice, so everything that
    /// needs the chip borrows the store's handle instead of building its own:
    /// the `spike-ota` build's OTA evidence, and card 222's one-shot read of
    /// the partition table and `otadata` for `GET /api/v1/status`'s `fw_slot`
    /// and `fw_state`.
    ///
    /// **Reads only.** Everything that writes to the settings partition goes
    /// through the methods below, so that the counters, the debounce and the
    /// `ERR_STORAGE` path stay in one place.
    pub fn raw(&mut self) -> &mut FlashStorage<'static> {
        &mut self.flash
    }

    /// The partition entries the boot-time table read found, by value.
    ///
    /// 68 bytes of `Copy`, so a caller takes them and stops borrowing this -
    /// which is what lets [`crate::http::read_fw_health`] hold the entry and
    /// the raw flash handle at the same time.
    pub fn parts(&self) -> Parts {
        self.parts
    }

    /// Read every setting. Never fails; see [`Store::load`].
    pub async fn load(&mut self) -> (Settings, LoadReport) {
        let Flash {
            flash,
            entry,
            scratch,
            ..
        } = self;
        let len = entry.len();
        let mut region = entry.as_flash_region(flash);
        let Ok(nor) = region.as_nor_flash() else {
            return (Settings::default(), LoadReport::default());
        };
        let Ok(mut store) = Store::new(BlockingAsync::new(Counted(nor)), 0..len) else {
            return (Settings::default(), LoadReport::default());
        };
        store.load(scratch.as_mut_slice()).await
    }

    /// Store the friendly name.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn save_name(&mut self, name: &Name) -> Result<Timing, StoreError> {
        timed!(with_store!(self, s, buf, s.save_name(buf, name).await))
    }

    /// Store the panel brightness.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn save_brightness(&mut self, level: u8) -> Result<Timing, StoreError> {
        timed!(with_store!(
            self,
            s,
            buf,
            s.save_brightness(buf, level).await
        ))
    }

    /// Store the idle mode.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn save_idle_mode(&mut self, mode: IdleMode) -> Result<Timing, StoreError> {
        timed!(with_store!(self, s, buf, s.save_idle_mode(buf, mode).await))
    }

    /// Store a credential pair. Written immediately, never debounced: the
    /// device is about to drop its association.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn save_wifi(&mut self, wifi: &Wifi) -> Result<Timing, StoreError> {
        timed!(with_store!(self, s, buf, s.save_wifi(buf, wifi).await))
    }

    /// Erase the whole partition: a factory reset, and the **only** thing that
    /// ever erases settings.
    ///
    /// `screeny-settings` deliberately refuses to do this on its own - a load
    /// that goes wrong falls back to defaults and keeps whatever is in flash -
    /// so the decision is the firmware's, and [`init`] takes it in exactly one
    /// situation. See [`repair`].
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn erase_all(&mut self) -> Result<(), StoreError> {
        let Flash { flash, entry, .. } = self;
        let len = entry.len();
        let mut region = entry.as_flash_region(flash);
        let Ok(nor) = region.as_nor_flash() else {
            return Err(StoreError::Flash);
        };
        let Ok(mut store) = Store::new(BlockingAsync::new(Counted(nor)), 0..len) else {
            return Err(StoreError::Internal);
        };
        store.erase_all().await
    }
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

/// What the boot path needs out of the partition table, all of it, once.
///
/// 68 bytes of `Copy`, passed by value from `main` to whoever wants a
/// partition. See [`read_partitions`] for why this exists rather than each
/// caller reading the table for itself.
#[derive(Clone, Copy, Default)]
pub struct Parts {
    /// The settings partition, `screeny`.
    pub settings: Option<PartitionEntry>,
    /// `otadata`, which card 222 reads for `fw_state` and card 241 will write.
    pub otadata: Option<PartitionEntry>,
    /// Flash offset of the partition this image is running from, read from
    /// the MMU. That is the *booted* slot and not otadata's selection, and the
    /// two differ exactly when a rollback has happened.
    pub booted_offset: Option<u32>,
}

/// Read the partition table **once for the whole boot**, and keep only the
/// 32-byte entries.
///
/// **Not `async`, and `#[inline(never)]`, on purpose.** The table is
/// `PARTITION_TABLE_MAX_LEN` = 3072 bytes; read inside an `async fn` that later
/// suspends, that buffer would be part of a task future, which is `.bss`, which
/// on this chip is taken out of core 0's main stack. Here it is an ordinary
/// stack frame that is gone before the caller's first `await`.
///
/// **And once rather than twice (card 243's stack lever).** Until 0.5.2 this
/// function found the `screeny` partition for [`init`] and
/// [`crate::http::read_fw_health`] read the whole table again for `fw_slot` and
/// `fw_state` - and the second copy was worse than the duplication suggests,
/// because that 3 KB buffer stayed alive *underneath* `Ota::new` and
/// `current_ota_state`, i.e. underneath esp-storage's own read path, which is
/// where the boot path's 13 KB high-water mark is made. Reading everything here
/// and handing on [`Parts`] means the buffer is gone before any of that runs,
/// and the deepest chain in the boot path loses 3,072 bytes from under it.
///
/// Everything is looked up **by label**, never by `partition_type()`, which
/// `unwrap!`s its conversion and would panic on a table entry with a subtype
/// this crate's enums do not know (research 006 section 3 - the same reason the
/// settings partition has always been found this way).
#[inline(never)]
fn read_partitions(flash: &mut FlashStorage<'static>) -> Parts {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = match partitions::read_partition_table(flash, &mut buf) {
        Ok(t) => t,
        Err(e) => {
            warn!("store: cannot read the partition table: {:?}", e);
            return Parts::default();
        }
    };
    Parts {
        settings: table.iter().find(|e| e.label_as_str() == LABEL),
        otadata: table.iter().find(|e| e.label_as_str() == OTADATA_LABEL),
        // `Err` here is "the MMU said something this crate could not map to a
        // partition", which is not a reason to fail a boot: `fw_slot` falls
        // back to otadata's selection and says `unknown` if that fails too.
        booted_offset: table.booted_partition().ok().flatten().map(|p| p.offset()),
    }
}

/// Open the store and read every setting.
///
/// Call this from `main` **before** the receiver core is built and before the
/// first composed frame, so the panel comes up at the stored brightness and
/// mDNS announces the stored name from its first announcement.
///
/// Load never fails: a missing partition, a blank one, a foreign schema version
/// or a flash error all produce the defaults plus a logged report, and the
/// device carries on.
pub async fn init(flash: esp_hal::peripherals::FLASH<'static>) -> (Settings, LoadReport) {
    // Auto-park, never ignore (research 006 section 4). At this point core 1 is
    // not running yet, and `pre_write` only parks a core that is - so an early
    // boot write costs nothing and a later one is correct.
    let mut flash = FlashStorage::new(flash).multicore_auto_park();

    // The one partition-table read of the whole boot (card 243). What the rest
    // of the firmware wants out of it travels as 68 bytes of `Parts`.
    let parts = read_partitions(&mut flash);

    let Some(entry) = parts.settings else {
        warn!(
            "store: no '{}' partition - settings are defaults and nothing will be saved",
            LABEL
        );
        return (Settings::default(), LoadReport::default());
    };
    info!(
        "store: '{}' partition at {:#x}, {} bytes ({} pages)",
        LABEL,
        entry.offset(),
        entry.len(),
        entry.len() / 4096
    );

    let mut f = Flash {
        flash,
        entry,
        scratch: Scratch::new(),
        parts,
    };
    let (mut settings, mut report) = f.load().await;
    if let Some((s, r)) = repair(&mut f, &report).await {
        settings = s;
        report = r;
    }
    *STORE.lock().await = Some(f);

    // Deliberately not a `Debug` of `Settings`: `Ssid`'s `Debug` prints the
    // SSID, and the Wi-Fi task's "connected" line is the only place in this
    // firmware that says one out loud.
    info!(
        "store: loaded schema {:?} fallback {:#06b} error {:?} | name {:?} brightness {} idle {} | wifi {}",
        report.schema,
        report.fallback.bits(),
        report.error,
        settings.name.as_str(),
        settings.brightness,
        settings.idle_mode.as_u8(),
        if settings.wifi.is_some() {
            "stored"
        } else {
            "none"
        },
    );
    (settings, report)
}

/// Erase a partition that does not hold a settings map at all, once, at boot.
///
/// **This is the one place the firmware erases settings**, and it is narrower
/// than it looks. It fires only on `Unreadable` + [`StoreError::Corrupted`],
/// which is the load telling us it could not even read the schema-version item:
/// `sequential-storage` has already re-run the operation through its own repair
/// pass (`run_with_auto_repair!`) and still says the region is not a map. A
/// *partly* damaged map does not come back like this - it comes back with some
/// keys at their defaults and a `fallback` bitset - and is left alone.
///
/// The case that actually produces it is a device whose `screeny` partition has
/// never been written: 0x410000 is inside the region the stock Tidbyt image
/// used, so the bytes there are not `0xFF` and are not ours. Card 212 hit it on
/// the first flash. The card's instruction is exactly this: a wrong settings
/// partition is fixed in code with `erase_all`, never with `espflash erase-*`.
///
/// Once per boot, and nothing here reboots, so it cannot become a loop. The
/// erase is one 64 KB region and it happens before the panel is up, so the
/// several hundred milliseconds core 1 spends parked are invisible.
///
/// Returns the re-read settings when it erased, `None` when it did not.
async fn repair(f: &mut Flash, report: &LoadReport) -> Option<(Settings, LoadReport)> {
    if !matches!(report.schema, SchemaState::Unreadable)
        || report.error != Some(StoreError::Corrupted)
    {
        return None;
    }
    warn!(
        "store: the '{}' partition does not hold a settings map (a partition that has never been written still holds whatever the stock image left there). Erasing it once, now.",
        LABEL
    );
    let t0 = Instant::now();
    let e0 = ERASES.load(Ordering::Relaxed);
    match f.erase_all().await {
        Ok(()) => {
            info!(
                "store: erase_all took {} us, {} sectors",
                t0.elapsed().as_micros() as u32,
                ERASES.load(Ordering::Relaxed).wrapping_sub(e0),
            );
            Some(f.load().await)
        }
        Err(e) => {
            warn!(
                "store: erase_all failed ({:?}) - running on defaults, nothing will be saved",
                e
            );
            None
        }
    }
}

/// Write a credential pair into an empty store.
///
/// Only the `bench-wifi` build has anything to seed with, and only a store that
/// holds no credentials at all is seeded - a `SET_WIFI` always outranks the
/// build. Seeding goes through the ordinary save path on purpose: from the very
/// first boot the device is running on stored credentials, so the path that
/// matters is the one being exercised.
///
/// Kept (rather than `#[cfg]`-ed away) in a default build so that the two
/// builds compile the same file: there is simply nothing to call it with, which
/// is the point.
#[cfg_attr(not(feature = "bench-wifi"), allow(dead_code))]
pub async fn seed_wifi(wifi: &Wifi) {
    let mut guard = STORE.lock().await;
    let Some(f) = guard.as_mut() else {
        return;
    };
    match f.save_wifi(wifi).await {
        Ok(t) => info!(
            "store: seeded the empty store from the build's credentials ({:?}, {} us, {} erases)",
            t.write, t.us, t.erases
        ),
        Err(e) => warn!("store: seeding failed: {:?}", e),
    }
}

// ---------------------------------------------------------------------------
// Immediate writes, for the opcodes that can answer ERR_STORAGE
// ---------------------------------------------------------------------------

/// A write a control handler asked for **before** its reply goes out.
///
/// Spec section 6.5's `ERR_STORAGE` is only honest for a write that has already
/// happened, so `SET_NAME` and `SET_WIFI` produce one of these and
/// [`crate::net::control_task`] performs it while the reply is still in its
/// buffer.
///
/// `Debug` is safe: [`Wifi`] derives it, and `Psk`'s own `Debug` prints a byte
/// count and nothing else (spec 8.4).
#[derive(Debug, Clone)]
pub enum Immediate {
    /// `SET_NAME`.
    Name(Name),
    /// `SET_WIFI`. `persist` is the request's bit 0; when it is clear the
    /// credentials are tried but not written.
    Wifi {
        /// The network to join.
        wifi: Wifi,
        /// Whether to write it to flash first.
        persist: bool,
    },
}

/// Carry out an [`Immediate`]. Returns `Ok(())` when there was nothing to write
/// as well as when the write succeeded.
///
/// # Errors
/// [`StoreError`], which the caller turns into `ERR_STORAGE`.
pub async fn commit_immediate(what: &Immediate) -> Result<(), StoreError> {
    let mut guard = STORE.lock().await;
    let Some(f) = guard.as_mut() else {
        return Err(StoreError::Flash);
    };
    let t = match what {
        Immediate::Name(name) => f.save_name(name).await?,
        Immediate::Wifi {
            wifi,
            persist: true,
        } => f.save_wifi(wifi).await?,
        // Section 8.2: without the persist bit the credentials are tried only
        // until reboot, so there is nothing to write and nothing to fail.
        Immediate::Wifi { persist: false, .. } => return Ok(()),
    };
    info!(
        "store: immediate write {:?} in {} us ({} erases)",
        t.write, t.us, t.erases
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The task
// ---------------------------------------------------------------------------

/// The debounced half: wake on a change, wait for the quiet period, commit.
///
/// On core 0, like every other task that touches flash. It reads the live value
/// out of wherever it already lives (the `BRIGHTNESS` atomic, the receiver's
/// `Core`) rather than keeping a copy, so the store can never disagree with what
/// the device is doing; and it **drops the `CORE` mutex before writing**, so a
/// 50 ms sector erase cannot also be 50 ms of the frame task waiting on a lock.
#[embassy_executor::task]
pub async fn store_task() {
    let mut deb = Debounce::new();
    loop {
        let now = Instant::now().as_millis();
        match deb.next_due_in_ms(now) {
            // Nothing pending: sleep until something changes.
            None => WAKE.wait().await,
            Some(0) => {}
            Some(ms) => {
                let _ = with_timeout(Duration::from_millis(ms), WAKE.wait()).await;
            }
        }

        let bits = DIRTY.swap(0, Ordering::Relaxed);
        let now = Instant::now().as_millis();
        if bits & DIRTY_NAME != 0 {
            deb.note_change(Field::Name, now);
        }
        if bits & DIRTY_BRIGHTNESS != 0 {
            deb.note_change(Field::Brightness, now);
        }
        if bits & DIRTY_IDLE != 0 {
            deb.note_change(Field::IdleMode, now);
        }

        while let Some(field) = deb.due(Instant::now().as_millis()) {
            commit(field).await;
        }
    }
}

/// Copy one live setting into flash.
async fn commit(field: Field) {
    // Read the live value first, with the CORE lock held for microseconds, and
    // let go of it before the flash call.
    let (name, idle) = {
        let guard = crate::net::CORE.lock().await;
        match guard.as_ref() {
            Some(core) => (
                Name::new(core.name()).unwrap_or_default(),
                core.idle_mode(),
            ),
            None => return,
        }
    };
    let brightness = crate::BRIGHTNESS.load(Ordering::Relaxed);

    let mut guard = STORE.lock().await;
    let Some(f) = guard.as_mut() else {
        return;
    };
    let r = match field {
        Field::Name => f.save_name(&name).await,
        Field::Brightness => f.save_brightness(brightness).await,
        Field::IdleMode => f.save_idle_mode(idle).await,
    };
    match r {
        Ok(t) => {
            if t.write == Write::Committed {
                info!(
                    "store: {:?} committed in {} us ({} erases; {} commits, {} skips since boot)",
                    field,
                    t.us,
                    t.erases,
                    COMMITS.load(Ordering::Relaxed),
                    SKIPS.load(Ordering::Relaxed),
                );
            }
        }
        // The reply this change came in on went out three seconds ago, so
        // `ERR_STORAGE` is not available: the log line and [`FAILURES`] are the
        // whole record, and card 222's status page reads the counter.
        Err(e) => warn!(
            "store: {:?} FAILED to commit: {:?} ({} failures since boot)",
            field,
            e,
            FAILURES.load(Ordering::Relaxed)
        ),
    }
}

// ---------------------------------------------------------------------------
// The bench self-test
// ---------------------------------------------------------------------------

/// Exercise the store on the device, by itself, and say what each write cost.
///
/// Off by default (`--features store-selftest`). It exists because a worker
/// cannot reach the device over the LAN to send `SET_*`, so without it the only
/// evidence that a flash write works, and what it does to the panel while it is
/// happening, would be the orchestrator's over-the-wire pass after a merge.
///
/// One run, at boot + 25 seconds:
///
/// 1. save a brightness, an idle mode and a name through the same [`Flash`]
///    methods the control handlers use, timing each and counting its sector
///    erases, then save the brightness again to show the skip path;
/// 2. print `render` max over the burst, so what the panel paid is in the same
///    log as what flash cost;
/// 3. reboot **once** and print what was loaded. The guard is the stored
///    *brightness*: pass 1 writes a value no default build would have, and any
///    boot that loads it takes the reporting branch instead. Pass 2 restores
///    the name and the idle mode (the name is the mDNS instance name, and a
///    device left on `Dim` shows nothing when the stream stops) and
///    deliberately leaves the brightness, so the next flash's boot log shows a
///    setting surviving a reflash as well as a reboot.
#[cfg(feature = "store-selftest")]
pub mod selftest {
    use embassy_time::Timer;

    use super::*;

    /// When the run starts. After the association, DHCP, mDNS and the Studio's
    /// reconnect (~15-20 s), so the writes land in the middle of a live 30 fps
    /// stream rather than in the quiet before one.
    const AT_S: u64 = 25;

    /// The name written by pass 1. It exercises the `SET_NAME` storage path,
    /// which is also the one that drives the mDNS instance name - and that is
    /// exactly why pass 2 puts it back.
    const MARKER_NAME: &str = "selftest-212";
    /// The brightness written by pass 1, and **the guard**: a boot that loads
    /// it knows the run has already happened and must not reboot again. 111 is
    /// not the default (96) and is well under the firmware cap (160).
    const MARKER_BRIGHTNESS: u8 = 111;
    /// The idle mode written by pass 1; restored in pass 2.
    const MARKER_IDLE: IdleMode = IdleMode::Dim;

    /// Log one write in the shape the card asks for.
    fn say(what: &str, r: &Result<Timing, StoreError>) {
        match r {
            Ok(t) => info!(
                "selftest: {} -> {:?} in {} us, {} sector erase(s)",
                what, t.write, t.us, t.erases
            ),
            Err(e) => warn!("selftest: {} FAILED {:?}", what, e),
        }
    }

    fn counters(when: &str) {
        info!(
            "selftest: counters {} | commits {} skips {} failures {} sector erases {} page writes {}",
            when,
            COMMITS.load(Ordering::Relaxed),
            SKIPS.load(Ordering::Relaxed),
            FAILURES.load(Ordering::Relaxed),
            ERASES.load(Ordering::Relaxed),
            PAGE_WRITES.load(Ordering::Relaxed),
        );
    }

    /// The whole run. Spawned only by the `store-selftest` build.
    #[embassy_executor::task]
    pub async fn selftest_task(loaded: Settings) {
        Timer::after(Duration::from_secs(AT_S)).await;

        // --- pass 2, or any later boot -------------------------------------
        if loaded.brightness == MARKER_BRIGHTNESS {
            if loaded.name.as_str() != MARKER_NAME {
                info!("selftest: already run on an earlier boot; nothing to do");
                return;
            }
            info!(
                "selftest: PASS 2, after the reboot - loaded name {:?} brightness {} idle {}; pass 1 wrote {:?} {} {}",
                loaded.name.as_str(),
                loaded.brightness,
                loaded.idle_mode.as_u8(),
                MARKER_NAME,
                MARKER_BRIGHTNESS,
                MARKER_IDLE.as_u8(),
            );
            info!(
                "selftest: all three settings survived the reboot: {}",
                if loaded.idle_mode.as_u8() == MARKER_IDLE.as_u8() {
                    "YES"
                } else {
                    "NO"
                }
            );
            counters("after the reboot");

            // Put back the two that would outstay their welcome, and leave the
            // brightness as the guard and as the next flash's evidence.
            let Ok(name) = Name::new("") else { return };
            let mut guard = STORE.lock().await;
            let Some(f) = guard.as_mut() else { return };
            let r = f.save_name(&name).await;
            say("restore the name to empty (screeny-<id>)", &r);
            let r = f.save_idle_mode(IdleMode::Status).await;
            say("restore the idle mode to Status", &r);
            info!(
                "selftest: done. Brightness {} is left in flash on purpose; the next boot, of any build, should load it.",
                MARKER_BRIGHTNESS
            );
            return;
        }

        // --- pass 1 --------------------------------------------------------
        info!(
            "selftest: PASS 1 - writing name {:?}, brightness {}, idle {}",
            MARKER_NAME,
            MARKER_BRIGHTNESS,
            MARKER_IDLE.as_u8()
        );
        let name = match Name::new(MARKER_NAME) {
            Ok(n) => n,
            Err(e) => {
                warn!("selftest: bad marker name {:?}", e);
                return;
            }
        };

        // `render` is measured on core 1, which is *parked* for the duration of
        // each ROM flash call, so it cannot report anything while a write is in
        // flight. What this window catches is the first render after the
        // unpark - the frame that ran late - which is the number that says what
        // the panel actually paid.
        crate::RENDER_US_MAX_WINDOW.store(0, Ordering::Relaxed);
        {
            let mut guard = STORE.lock().await;
            let Some(f) = guard.as_mut() else {
                warn!("selftest: no store - nothing to test");
                return;
            };
            let r = f.save_brightness(MARKER_BRIGHTNESS).await;
            say("save_brightness", &r);
            let r = f.save_idle_mode(MARKER_IDLE).await;
            say("save_idle_mode", &r);
            let r = f.save_name(&name).await;
            say("save_name", &r);
            // The same value again: this is the path a debounced slider that
            // landed back where it started takes, and it must cost no erase and
            // no write at all.
            let r = f.save_brightness(MARKER_BRIGHTNESS).await;
            say("save_brightness again (must Skip)", &r);
        }
        // Let core 1 run again, then report what the burst cost the panel.
        Timer::after(Duration::from_millis(500)).await;
        info!(
            "selftest: render max over the whole write burst: {} us",
            crate::RENDER_US_MAX_WINDOW.load(Ordering::Relaxed)
        );
        counters("before the reboot");

        // Two telemetry periods, so the log shows the stream either side of the
        // writes, then reboot exactly once.
        Timer::after(Duration::from_secs(11)).await;
        info!("selftest: rebooting once to prove the values survive");
        Timer::after(Duration::from_millis(100)).await;
        esp_hal::system::software_reset();
    }
}
