//! How much of core 0's main stack is actually used.
//!
//! On this chip `.stack` is not a configured size. `.data`, `.bss` and core
//! 0's main stack come out of one DRAM region, and the stack is simply
//! whatever the linker has left between `_bss_end` (== `_stack_end_cpu0`) and
//! `_stack_start_cpu0` at 0x3ffe0000. `xtensa-esp32-elf-size -A` prints that
//! remainder as `.stack`, but a remainder is a *ceiling*, not a measurement:
//! it says nothing about how close we are to the stack guard. Card 201 wanted
//! to add 23 KB of `.bss` for a web server and a soft-AP, which would have cut
//! the remainder to 13.7 KB, and nobody could say whether that was fatal.
//!
//! So: measure. At the very top of `main`, before anything deep has run, paint
//! the whole region below the current stack pointer with a pattern. Later,
//! scan it upward from the bottom; the first word that is no longer the
//! pattern is the deepest the stack has ever reached, interrupts included
//! (on this chip an ISR runs on whatever stack is current).
//!
//! Cost: one pass over ~36 KB at boot and one pass at the 60 s mark. Neither
//! is on any hot path, so this lives in the default firmware rather than
//! behind a feature — the number is only useful if it is the number the
//! shipping build produces.

use core::sync::atomic::{AtomicBool, Ordering};

/// The paint. Any value a real frame is unlikely to hold; a false match only
/// ever makes the reported high-water *smaller*, and one word of coincidence
/// four bytes below the true low-water mark would still be within rounding.
const PAINT: u32 = 0x5741_5445; // "WATE"

/// Bytes at the bottom of the region left unpainted.
///
/// esp-rtos snapshots the main task's stack guard from
/// `_stack_end_cpu0 + ESP_HAL_CONFIG_STACK_GUARD_OFFSET` and compares it on
/// every context switch; on this build that word sits 60 bytes above the
/// bottom. Painting it would still work (esp-rtos snapshots whatever it
/// finds), but leaving the bottom kilobyte alone keeps the guard, and the
/// panic message that quotes it, exactly as it was.
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
/// 24 KB of framebuffer temporaries this card exists to remove; the local sat
/// near the *top* of that frame, and the paint went straight through the live
/// frame and its saved return addresses. The device panic-looped on the first
/// poll of `main` with `esp_sync: lock is not reentrant`. Hence
/// [`frame_mark`].
const FRAME_MARGIN: usize = 512;

unsafe extern "C" {
    /// Low address: the bottom of the region, == `_bss_end`.
    static _stack_end_cpu0: u8;
    /// High address: 0x3ffe0000, where the stack starts and grows down.
    static _stack_start_cpu0: u8;
}

static PAINTED: AtomicBool = AtomicBool::new(false);

fn bottom() -> usize {
    (&raw const _stack_end_cpu0) as usize
}

fn top() -> usize {
    (&raw const _stack_start_cpu0) as usize
}

/// Total bytes the linker left for core 0's main stack: the `.stack` section.
pub fn size() -> usize {
    top() - bottom()
}

/// An address inside the frame of a function that holds nothing.
///
/// Reading `a1` directly would be exact, but inline assembly on Xtensa still
/// needs `#![feature(asm_experimental_arch)]` and the firmware does not gate
/// on nightly features for one probe. This is the next best thing and it is
/// provable rather than hopeful: `#[inline(never)]` means this frame is its
/// own, it holds one word, and it is a *callee* of [`paint`], so its stack
/// pointer is strictly below `paint`'s. Subtracting [`FRAME_MARGIN`] from a
/// local inside it therefore lands below every frame that is live — and below
/// the 16-byte Xtensa register-window spill area, which is the only thing the
/// hardware writes under a stack pointer.
#[inline(never)]
fn frame_mark() -> usize {
    let probe = 0u32;
    core::hint::black_box((&raw const probe) as usize)
}

/// Fill the unused part of the region with [`PAINT`].
///
/// Call this as the first statement of `main`, before `esp_hal::init` and
/// before `esp_rtos::start`: everything after it is then measurable.
#[inline(never)]
pub fn paint() {
    let lo = bottom() + GUARD_RESERVE;
    let hi = (frame_mark() - FRAME_MARGIN) & !3;
    if hi <= lo {
        return;
    }

    let mut p = lo;
    while p < hi {
        // SAFETY: [lo, hi) is inside the linker's stack region for this core,
        // strictly below this function's own frame and strictly above the
        // guard reserve, so no live data lives there.
        unsafe { (p as *mut u32).write_volatile(PAINT) };
        p += 4;
    }
    PAINTED.store(true, Ordering::Release);
}

/// Deepest the stack has been since [`paint`], in bytes below
/// `_stack_start_cpu0`.
///
/// Returns `None` if [`paint`] never ran. A return value equal to
/// `size() - GUARD_RESERVE` means the paint was exhausted, i.e. the stack has
/// been at least that deep — but it would have hit the guard first.
pub fn high_water() -> Option<usize> {
    if !PAINTED.load(Ordering::Acquire) {
        return None;
    }
    let lo = bottom() + GUARD_RESERVE;
    let top = top();
    let mut p = lo;
    while p < top {
        // SAFETY: reading inside our own stack region, word aligned.
        if unsafe { (p as *const u32).read_volatile() } != PAINT {
            break;
        }
        p += 4;
    }
    Some(top - p)
}

/// Bytes of paint still standing below the high-water mark: the headroom
/// between the deepest frame so far and the guard reserve.
pub fn headroom() -> Option<usize> {
    high_water().map(|hw| size().saturating_sub(hw + GUARD_RESERVE))
}
