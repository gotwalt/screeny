//! How much of each core's stack is actually used.
//!
//! On this chip core 0's `.stack` is not a configured size. `.data`, `.bss`
//! and core 0's main stack come out of one DRAM region, and the stack is
//! simply whatever the linker has left between `_bss_end` (==
//! `_stack_end_cpu0`) and `_stack_start_cpu0` at 0x3ffe0000.
//! `xtensa-esp32-elf-size -A` prints that remainder as `.stack`, but a
//! remainder is a *ceiling*, not a measurement: it says nothing about how
//! close we are to the stack guard. Card 201 wanted to add 23 KB of `.bss`
//! for a web server and a soft-AP, which would have cut the remainder to
//! 13.7 KB, and nobody could say whether that was fatal.
//!
//! Core 1's stack **is** a configured size - the [`crate::APP_CORE_STACK`]
//! static - and until card 227 it had never been measured at all. It is
//! ordinary `.bss`, so every byte of it is a byte off core 0's `.stack`:
//! over-provisioning it is not free, it is a transfer.
//!
//! So: measure, the same way for both. Paint the unused part of the region
//! with a pattern before anything deep has run, then scan it upward from the
//! bottom; the first word that is no longer the pattern is the deepest that
//! stack has ever reached, interrupts included. Interrupts really are
//! included and that is not an accident of this chip: `xtensa-lx-rt`'s
//! `SAVE_CONTEXT` begins `addmi sp, sp, -256` on **the stack that was
//! interrupted**, so every interrupt level costs a 256-byte context frame
//! plus its handler's own frames, wherever it lands.
//!
//! ## Three readings, not one
//!
//! Card 220 scanned once, at the 60 s telemetry tick, and reported a number.
//! Card 222 then found real TCP traffic pushing core 0 far deeper than the
//! in-memory router test had, and the orchestrator found the mark still
//! creeping an hour later with nothing but the stream, a WiFi rejoin and a
//! handful of requests. A single reading cannot say *what* went deep.
//!
//! So [`watch_task`] samples every [`WATCH_MS`] milliseconds and logs only
//! when a mark has **grown**, with the delta. The log volume is bounded by
//! the growth itself - a high-water mark is monotonic, so a line is only ever
//! printed when something genuinely went deeper than anything before it - and
//! a sample costs a few thousand word reads (see [`Region::high_water`]),
//! which at 4 Hz is not measurable. Lined up against the timestamps of the
//! serial log's other lines (picoserve logs every accepted connection, the
//! WiFi task logs every join and disconnect) that is what turns "18 KB" into
//! "18 KB, and here is where".

use core::sync::atomic::{AtomicUsize, Ordering};

use embassy_time::{Duration, Timer};
use log::info;

/// The paint. Any value a real frame is unlikely to hold; a false match only
/// ever makes the reported high-water *smaller*, and one word of coincidence
/// four bytes below the true low-water mark would still be within rounding.
const PAINT: u32 = 0x5741_5445; // "WATE"

/// Bytes at the bottom of a region left unpainted.
///
/// esp-rtos snapshots a task's stack guard from
/// `stack_bottom + ESP_HAL_CONFIG_STACK_GUARD_OFFSET` and compares it on
/// every context switch; on this build that word sits 60 bytes above the
/// bottom. Painting it would still work (esp-rtos snapshots whatever it
/// finds), but leaving the bottom kilobyte alone keeps the guard, and the
/// panic message that quotes it, exactly as it was.
///
/// It also means a reported high-water of `size() - GUARD_RESERVE` means "at
/// least this deep, and the paint ran out" rather than an exact number. That
/// has never happened on either core.
const GUARD_RESERVE: usize = 1024;

/// How far below the stack pointer to stop painting.
///
/// Slack for the register-window spill area and for anything the compiler put
/// just under `a1`. It costs nothing: it only leaves the top half-kilobyte of
/// the region unpainted, and a scan that runs all the way up to it reports
/// "never went below here", which is the honest answer.
///
/// **The bound has to be below the frame it is measured from, and the only way
/// to know that is to measure it in a frame you own.** The first version of
/// this file took the address of a local in `paint` itself. `paint` was
/// inlined into `main`, whose poll frame in the `fb-on-stack` build is the
/// 24 KB of framebuffer temporaries card 220 exists to remove; the local sat
/// near the *top* of that frame, and the paint went straight through the live
/// frame and its saved return addresses. The device panic-looped on the first
/// poll of `main` with `esp_sync: lock is not reentrant`. Hence
/// [`frame_mark`].
const FRAME_MARGIN: usize = 512;

/// How often [`watch_task`] looks for growth.
///
/// Fast enough that a line lands next to the log line of whatever caused it -
/// an accepted HTTP connection, a join, a disconnect - and slow enough to be
/// free: a sample that finds no growth is a single `read_volatile`.
const WATCH_MS: u64 = 250;

unsafe extern "C" {
    /// Low address: the bottom of core 0's region, == `_bss_end`.
    static _stack_end_cpu0: u8;
    /// High address: 0x3ffe0000, where core 0's stack starts and grows down.
    static _stack_start_cpu0: u8;
}

/// One painted stack, and the deepest anything has been on it.
///
/// Every field is an atomic because the two cores share the struct: core 1
/// runs on [`CORE1`]'s region while core 0 scans it. Nothing here writes to
/// the region after [`Region::paint`], so the scan is a read of memory whose
/// only other writer is the stack itself - and the value it is looking for
/// can only ever be destroyed, never restored, which is what makes the
/// remembered cursor sound.
pub struct Region {
    bottom: AtomicUsize,
    top: AtomicUsize,
    /// The last high-water [`Region::grown`] reported, so it can report only
    /// changes.
    reported: AtomicUsize,
}

impl Region {
    pub const fn new() -> Self {
        Self {
            bottom: AtomicUsize::new(0),
            top: AtomicUsize::new(0),
            reported: AtomicUsize::new(0),
        }
    }

    fn armed(&self) -> Option<(usize, usize)> {
        let bottom = self.bottom.load(Ordering::Acquire);
        let top = self.top.load(Ordering::Acquire);
        (bottom != 0 && top > bottom).then_some((bottom, top))
    }

    /// Total bytes in the region.
    pub fn size(&self) -> usize {
        self.armed().map_or(0, |(b, t)| t - b)
    }

    /// Fill `[bottom + GUARD_RESERVE, hi)` with [`PAINT`] and start measuring.
    ///
    /// # Safety
    ///
    /// `bottom..top` must be a stack region, and `hi` must be strictly below
    /// every frame that is live on it. For core 0 that means `hi` comes from
    /// [`frame_mark`]; for core 1 it means the paint happens before the core
    /// is started, when nothing is live on it at all.
    unsafe fn paint(&self, bottom: usize, top: usize, hi: usize) {
        let lo = bottom + GUARD_RESERVE;
        let hi = hi.min(top) & !3;
        if hi <= lo {
            return;
        }
        let mut p = lo;
        while p < hi {
            // SAFETY: the caller's contract - inside the region, strictly
            // below anything live, strictly above the guard reserve.
            unsafe { (p as *mut u32).write_volatile(PAINT) };
            p += 4;
        }
        self.bottom.store(bottom, Ordering::Relaxed);
        // Last, and with a release: it is what [`armed`] tests.
        self.top.store(top, Ordering::Release);
    }

    /// Deepest this stack has been since it was painted, in bytes below the
    /// top. `None` if it was never painted.
    ///
    /// **Scan up from the bottom, and stop at the first word that is no longer
    /// [`PAINT`].** Not down from the last answer, which is the optimisation
    /// this code had for one build and which is wrong twice over: the mark
    /// moves *down* as the stack deepens, so a cursor cannot be resumed
    /// upward at all, and a downward resume walks newly-written memory, where
    /// a word that coincidentally holds the paint value would stop it early
    /// and under-report. Scanning up from the bottom only ever crosses memory
    /// that really is still painted, so it lands on the true mark.
    ///
    /// The cost is one word read per byte-quad of *unused* stack - a few
    /// thousand reads, tens of microseconds - which is why [`watch_task`] can
    /// afford it four times a second and still be invisible.
    pub fn high_water(&self) -> Option<usize> {
        let (bottom, top) = self.armed()?;
        let mut p = bottom + GUARD_RESERVE;
        while p < top {
            // SAFETY: reading inside a stack region we painted, word aligned.
            if unsafe { (p as *const u32).read_volatile() } != PAINT {
                break;
            }
            p += 4;
        }
        Some(top - p)
    }

    /// Bytes of paint still standing below the high-water mark: the headroom
    /// between the deepest frame so far and the guard reserve.
    pub fn headroom(&self) -> Option<usize> {
        self.high_water()
            .map(|hw| self.size().saturating_sub(hw + GUARD_RESERVE))
    }

    /// `Some((high_water, delta))` the first time, and afterwards only when
    /// the mark has moved.
    pub fn grown(&self) -> Option<(usize, usize)> {
        let hw = self.high_water()?;
        let was = self.reported.load(Ordering::Relaxed);
        (hw > was).then(|| {
            self.reported.store(hw, Ordering::Relaxed);
            (hw, hw - was)
        })
    }
}

/// Core 0's main stack: the `.stack` section, the remainder of main DRAM.
pub static CORE0: Region = Region::new();

/// Core 1's stack: [`crate::APP_CORE_STACK`], ordinary `.bss`.
pub static CORE1: Region = Region::new();

/// An address inside the frame of a function that holds nothing.
///
/// Reading `a1` directly would be exact, but inline assembly on Xtensa still
/// needs `#![feature(asm_experimental_arch)]` and the firmware does not gate
/// on nightly features for one probe. This is the next best thing and it is
/// provable rather than hopeful: `#[inline(never)]` means this frame is its
/// own, it holds one word, and it is a *callee* of [`paint_core0`], so its
/// stack pointer is strictly below that caller's. Subtracting
/// [`FRAME_MARGIN`] from a local inside it therefore lands below every frame
/// that is live - and below the 16-byte Xtensa register-window spill area,
/// which is the only thing the hardware writes under a stack pointer.
#[inline(never)]
fn frame_mark() -> usize {
    let probe = 0u32;
    core::hint::black_box((&raw const probe) as usize)
}

/// Paint core 0's main stack.
///
/// Call this as the first statement of `main`, before `esp_hal::init` and
/// before `esp_rtos::start`: everything after it is then measurable.
#[inline(never)]
pub fn paint_core0() {
    let bottom = (&raw const _stack_end_cpu0) as usize;
    let top = (&raw const _stack_start_cpu0) as usize;
    // SAFETY: the linker's own bounds for this core's stack, and `frame_mark`
    // returns an address below this function's frame. See [`frame_mark`].
    unsafe { CORE0.paint(bottom, top, frame_mark() - FRAME_MARGIN) };
}

/// Paint core 1's stack.
///
/// # Safety
///
/// Call it from core 0 with core 1 **not yet started**, and with `bottom` and
/// `top` taken from the `esp_hal::system::Stack` that is about to be handed
/// to `esp_rtos::start_second_core`. Nothing is live on that memory until the
/// core starts, so the whole region is paintable; esp-hal then copies the
/// entry closure near the top of it and esp-rtos writes its guard word near
/// the bottom, and both of those are honestly counted as "used".
#[cfg_attr(feature = "display-on-core0", allow(dead_code))]
pub unsafe fn paint_core1(bottom: usize, top: usize) {
    // SAFETY: the caller's contract - core 1 has not started, so no frame is
    // live anywhere in the region.
    unsafe { CORE1.paint(bottom, top, top) };
}

/// Log a line whenever either core's stack goes deeper than it ever has.
///
/// In the **default** firmware, not behind a feature, for the same reason the
/// paint is: a stack number is only worth having if it is the number the
/// shipping build produces. The cost is one `read_volatile` per core per
/// [`WATCH_MS`], and a line only when the mark actually moves.
#[embassy_executor::task]
pub async fn watch_task() {
    loop {
        Timer::after(Duration::from_millis(WATCH_MS)).await;
        if let Some((hw, delta)) = CORE0.grown() {
            info!(
                "stack: core 0 high-water {} of {} (+{}), {} free",
                hw,
                CORE0.size(),
                delta,
                CORE0.headroom().unwrap_or(0),
            );
        }
        if let Some((hw, delta)) = CORE1.grown() {
            info!(
                "stack: core 1 high-water {} of {} (+{}), {} free",
                hw,
                CORE1.size(),
                delta,
                CORE1.headroom().unwrap_or(0),
            );
        }
    }
}
