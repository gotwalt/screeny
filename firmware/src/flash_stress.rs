//! Card 245's bench build: hammer the flash path until it wedges, or prove it
//! will not.
//!
//! The wedge this card fixes is one coin flip per flash operation, weighted by
//! how much of the time core 1 happens to be inside a cross-core critical
//! section. That makes it a *statistical* claim, and a statistical claim needs
//! trials. Before this build there was exactly one way to run a trial - upload
//! a megabyte over HTTP and watch - which costs twenty-five seconds of
//! attention per five hundred flips, produces one bit of evidence,
//! and confounds the flash path with the socket, the scanner, the claim, the
//! panel and the radio all at once.
//!
//! This is the flash path by itself, at full speed, with an argument attached.
//!
//! ## What it does, and why each choice
//!
//! - **Only [`store::InactiveSlot`] sectors, ever.** The target comes from
//!   [`crate::store::inactive_slot`], which is the only thing that can make one
//!   and only makes one after comparing the entry's offset with the MMU's
//!   answer to "which partition am I running from". The offset comes from
//!   [`screeny_fwimage::stress_sector`], which a host test walks four times
//!   round a 2 MB slot (`crates/fwimage/tests/checks.rs`,
//!   `stress_sector_stays_inside_the_slot`), and it is checked again by
//!   [`plan_write`] and a third time by `esp-bootloader-esp-idf`'s own
//!   partition-relative bounds check. The running slot, the bootloader, the
//!   partition table, `otadata` and `screeny` are not addressable from here.
//!
//! - **It walks forwards and wraps**, one sector per cycle, so five hundred
//!   cycles spread their erase wear over five hundred sectors of a slot rated
//!   for ~100,000 cycles each. Hammering one sector would be a different and
//!   much less interesting experiment.
//!
//! - **It refuses to run when the slot is spoken for**: on a trial boot the
//!   inactive slot holds the image this device may have to roll back to
//!   (card 241, research 006 section 6), and during an upload or an activation
//!   somebody else is writing it. In all three cases this says so and stops,
//!   because a bench build that quietly destroys the escape hatch is worse than
//!   no bench build.
//!
//! - **Dither stays on and the panel keeps its picture.** An upload turns
//!   dither off and puts a static screen up, which parks the display task and
//!   makes core 1 quiet - and a quiet core 1 is a core 1 that is rarely inside
//!   a critical section, which is to say *the easy case*. This build leaves the
//!   stream and the dither running on purpose, so core 1 is taking the
//!   scheduler lock as often as it ever does and every cycle is a coin flip at
//!   its worst odds. A run that survives here survives an upload.
//!
//! - **It reads back what it wrote** and compares, so a cycle that silently did
//!   nothing is counted rather than believed.
//!
//! - **The logging is bounded**: one line every [`REPORT_EVERY`] cycles and one
//!   at the end, never one per cycle. Five hundred `info!`s through a blocking
//!   UART at 230,400 baud would be seconds of core 0 spent describing the thing
//!   it is measuring - card 240's rule, and the same reason an upload logs once.
//!
//! ## What a pass and a failure look like
//!
//! Pass: the run reaches `flash-stress: done` with `mismatches 0`, and the
//! `swaps +N` on that line is large - core 1 refreshed the panel all the way
//! through. Failure is not a log line at all: the device goes silent and comes
//! back on `rst:0x7 (TG0WDT_SYS_RESET)` about twenty seconds later, exactly as
//! an upload did before this card. The next boot's breadcrumb will *not* say
//! "a firmware upload was in flight", because this is not one - the serial log
//! and the reset reason are the evidence.

use alloc::boxed::Box;
use core::sync::atomic::Ordering;

use embassy_time::{Duration, Instant, Timer};
use embedded_storage::nor_flash::{NorFlash as _, ReadNorFlash as _};
use log::{info, warn};
use screeny_fwimage::{plan_write, stress_sector, SECTOR};

use crate::ota;
use crate::store;

/// How long after boot the run starts.
///
/// Long enough for the association, DHCP, mDNS and the Studio's first frames,
/// so the run happens with a stream on the wire rather than on a device that is
/// still coming up. The stream is started before flashing.
const START_AFTER_S: u64 = 30;

/// How many erase + write + read-back cycles one run does.
///
/// Five hundred is two more coin flips than the upload that wedged three times
/// (248 sectors), so a clean run is *more* evidence than a clean upload, and at
/// ~50 ms a cycle it is about twenty-five seconds - a minute of bench time for
/// three runs.
const CYCLES: u32 = 500;

/// One progress line per this many cycles: five lines a run.
const REPORT_EVERY: u32 = 100;

/// What one run measured.
struct Stats {
    cycles: u32,
    mismatches: u32,
    erase_total: u64,
    erase_min: u32,
    erase_max: u32,
    write_total: u64,
    write_min: u32,
    write_max: u32,
    read_total: u64,
    /// The longest single guarded window, whichever operation it was. This is
    /// the number that says how long core 0's interrupts were masked and core 1
    /// was stalled in the worst case - the thing research 006 section 4 called
    /// the open risk.
    window_max: u32,
    /// The longest whole cycle, including the lock and the read-back.
    cycle_max: u32,
}

impl Stats {
    const fn new() -> Self {
        Self {
            cycles: 0,
            mismatches: 0,
            erase_total: 0,
            erase_min: u32::MAX,
            erase_max: 0,
            write_total: 0,
            write_min: u32::MAX,
            write_max: 0,
            read_total: 0,
            window_max: 0,
            cycle_max: 0,
        }
    }

    fn note(&mut self, erase_us: u32, write_us: u32, read_us: u32, cycle_us: u32) {
        self.cycles += 1;
        self.erase_total += u64::from(erase_us);
        self.erase_min = self.erase_min.min(erase_us);
        self.erase_max = self.erase_max.max(erase_us);
        self.write_total += u64::from(write_us);
        self.write_min = self.write_min.min(write_us);
        self.write_max = self.write_max.max(write_us);
        self.read_total += u64::from(read_us);
        self.window_max = self.window_max.max(erase_us).max(write_us);
        self.cycle_max = self.cycle_max.max(cycle_us);
    }

    fn mean(total: u64, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            (total / u64::from(n)) as u32
        }
    }
}

/// One 4 KB sector, word aligned for the same reason `ota::Sector` is: a
/// misaligned buffer makes `esp-storage` copy through *another* 4,096-byte
/// buffer on the caller's stack, and this runs on core 0's.
#[repr(C, align(4))]
struct Sector([u8; SECTOR]);

/// The pattern written into sector `n`.
///
/// Not a constant, and not all-`0xFF`: an erase leaves `0xFF`, so a pattern of
/// `0xFF` would make "the write did nothing" and "the write worked" look
/// identical on the read-back. A per-cycle byte makes a stale sector show up as
/// a mismatch too.
fn fill(buf: &mut Sector, cycle: u32) {
    let b = (cycle & 0xFF) as u8;
    for (i, slot) in buf.0.iter_mut().enumerate() {
        // Two bytes that both move: the cycle, and the position within the
        // sector, so a short write shows up as well as a stale one.
        *slot = b ^ (i as u8);
    }
}

/// The bench run. Spawned by `main` only in a `flash-stress` build.
#[embassy_executor::task]
pub async fn stress_task() {
    Timer::after(Duration::from_secs(START_AFTER_S)).await;

    // The three "somebody else owns the slot" refusals. `on_trial` is the one
    // that matters: staging over the rollback image would leave this device
    // with one unproven image and nowhere to go.
    if ota::boot_class().on_trial() {
        warn!("flash-stress: this is a trial boot - the inactive slot is the rollback image, so nothing will be written");
        return;
    }
    if ota::updating() || ota::activating() {
        warn!("flash-stress: an upload owns the slot - not running");
        return;
    }
    let Some(slot) = store::inactive_slot().await else {
        warn!("flash-stress: no inactive app slot - nothing safe to write, not running");
        return;
    };
    let Some(mut buf) = alloc_sector() else {
        warn!("flash-stress: no room on the heap for a 4 KB buffer - not running");
        return;
    };

    info!(
        "flash-stress: BENCH BUILD - {} erase+write+read cycles into the INACTIVE slot at {:#x} ({} KB), one sector each, stream and dither left running",
        CYCLES,
        slot.offset(),
        slot.len() / 1024,
    );

    let swaps0 = crate::SWAPS.load(Ordering::Relaxed);
    let t0 = Instant::now();
    let mut stats = Stats::new();

    for cycle in 0..CYCLES {
        // Card 241b's liveness feed, once per cycle, for the same reason the
        // staging loop feeds it: a pathologically slow erase (the data sheet
        // allows 400 ms where this bench measures 40) must not be mistaken for
        // the wedge this build is hunting.
        ota::feed();

        let Some(offset) = stress_sector(slot.len(), cycle) else {
            warn!("flash-stress: the slot holds no whole sector - stopping");
            break;
        };
        // The same second lock the staging loop turns, on the same arithmetic.
        if plan_write(slot.len(), offset, SECTOR).is_err() {
            warn!("flash-stress: refused offset {:#x} - stopping", offset);
            break;
        }
        fill(&mut buf, cycle);

        let c0 = Instant::now();
        let outcome = {
            // The store's lock and the store's `FlashStorage`, per cycle, so
            // a settings commit waits one cycle and not the whole run - the
            // staging loop's rule 3, unchanged.
            let mut guard = store::STORE.lock().await;
            let Some(f) = guard.as_mut() else {
                warn!("flash-stress: no flash handle - stopping");
                break;
            };
            let entry = slot.entry();
            one_cycle(entry, f.raw(), offset, &mut buf)
        };
        let cycle_us = c0.elapsed().as_micros() as u32;

        match outcome {
            Ok(Cycle {
                erase_us,
                write_us,
                read_us,
                matched,
            }) => {
                if !matched {
                    stats.mismatches += 1;
                }
                stats.note(erase_us, write_us, read_us, cycle_us);
            }
            Err(()) => {
                warn!("flash-stress: a flash call failed at {:#x} - stopping", offset);
                break;
            }
        }

        if stats.cycles.is_multiple_of(REPORT_EVERY) {
            info!(
                "flash-stress: {} of {} cycles, {} mismatches, longest masked window {} us, longest cycle {} us",
                stats.cycles, CYCLES, stats.mismatches, stats.window_max, stats.cycle_max,
            );
        }
    }

    let wall_ms = t0.elapsed().as_millis() as u32;
    let swaps = crate::SWAPS.load(Ordering::Relaxed).wrapping_sub(swaps0);
    info!(
        "flash-stress: done - {} cycles, {} mismatches, {} ms wall | erase us min/mean/max {}/{}/{} | write us {}/{}/{} | read us mean {} | longest masked window {} us | longest cycle {} us | core 1 swaps +{}",
        stats.cycles,
        stats.mismatches,
        wall_ms,
        if stats.cycles == 0 { 0 } else { stats.erase_min },
        Stats::mean(stats.erase_total, stats.cycles),
        stats.erase_max,
        if stats.cycles == 0 { 0 } else { stats.write_min },
        Stats::mean(stats.write_total, stats.cycles),
        stats.write_max,
        Stats::mean(stats.read_total, stats.cycles),
        stats.window_max,
        stats.cycle_max,
        swaps,
    );
}

/// What one cycle cost, and whether what came back is what went in.
struct Cycle {
    erase_us: u32,
    write_us: u32,
    read_us: u32,
    matched: bool,
}

/// Erase, write and read back one sector.
///
/// **Synchronous and `#[inline(never)]`**, exactly like [`crate::ota`]'s
/// `stage_sector`: everything `esp-storage` puts on the stack below here lives
/// in a real frame that is gone before the caller's next `await`, rather than
/// in this task's future, which would be `.bss`, which is core 0's stack.
///
/// Every erase and every program goes through [`store::guarded`] - the whole
/// point of the build is to exercise *that* path, at its own call granularity,
/// so the bench is measuring the thing it is trying to prove. The read is
/// deliberately not guarded: `esp-storage`'s read never parks core 1, so there
/// is no window there, and this build should not accidentally test a wider
/// discipline than the one the firmware ships.
#[inline(never)]
fn one_cycle(
    entry: esp_bootloader_esp_idf::partitions::PartitionEntry,
    flash: &mut esp_bootloader_esp_idf::partitions::FlashStorage<'static>,
    offset: u32,
    buf: &mut Sector,
) -> Result<Cycle, ()> {
    let mut region = entry.as_flash_region(flash);

    let e0 = Instant::now();
    store::guarded(|| region.erase(offset, offset + SECTOR as u32)).map_err(|e| {
        warn!("flash-stress: erase at {:#x} failed: {:?}", offset, e);
    })?;
    let erase_us = e0.elapsed().as_micros() as u32;

    let mut nor = region.as_nor_flash().map_err(|e| {
        warn!("flash-stress: the slot has no NorFlash view: {:?}", e);
    })?;

    let w0 = Instant::now();
    store::guarded(|| nor.write(offset, &buf.0)).map_err(|e| {
        warn!("flash-stress: write at {:#x} failed: {:?}", offset, e);
    })?;
    let write_us = w0.elapsed().as_micros() as u32;

    // Read back into the *same* buffer: comparing against what was written
    // means keeping a copy, and a second 4 KB buffer is 4 KB of core 0's stack
    // this file is not going to spend. The check is therefore against the
    // pattern rule rather than against a saved copy, which is the same test.
    let expect = buf.0[0];
    let r0 = Instant::now();
    nor.read(offset, &mut buf.0).map_err(|e| {
        warn!("flash-stress: read-back at {:#x} failed: {:?}", offset, e);
    })?;
    let read_us = r0.elapsed().as_micros() as u32;

    let matched = buf
        .0
        .iter()
        .enumerate()
        .all(|(i, b)| *b == expect ^ (i as u8));
    if !matched {
        warn!(
            "flash-stress: read-back at {:#x} does not match what was written",
            offset
        );
    }

    Ok(Cycle {
        erase_us,
        write_us,
        read_us,
        matched,
    })
}

/// One aligned 4 KB sector on the heap, or `None`.
///
/// `Box::new(Sector([0; SECTOR]))` would build the value as a 4 KB temporary on
/// the stack and then move it; `alloc_zeroed` writes straight into the
/// allocation. The same reasoning, and the same code, as `ota::alloc_sector` -
/// duplicated rather than shared because `ota`'s is part of the upload's
/// buffer discipline and this build must not be able to perturb it.
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
