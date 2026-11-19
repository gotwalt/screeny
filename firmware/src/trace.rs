//! Where core 0's tasks last were, kept where a reset cannot reach.
//!
//! The liveness watchdog is the right answer to a hung core 0 and the wrong
//! witness: it resets the chip and leaves nothing behind but `last_reset: wdt`.
//! A panic leaves `last_panic`, but for a CPU exception on core 1 that says
//! where core 1 was and nothing about core 0. A debugger is not on this bench,
//! and a log line cannot be printed from the inside of a hang. So each suspect
//! writes one small number into RTC slow memory as it goes, and the next boot
//! keeps what they said last and serves it as `last_trace` on
//! `GET /api/v1/panic` (card 378; first built as a bench diagnostic for the
//! upload hang of 2026-11-07).
//!
//! A plain volatile write and a timer read, so it is cheap enough for the
//! frame task. It costs **no `.bss`**: the live record and the previous
//! run's copy are both RTC slow memory, 80 bytes of it.
//!
//! **The step numbers are this firmware's own**, written next to the
//! `mark` calls; read them against the source of the build that wrote them.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use embassy_time::Instant;
use log::info;
use screeny_device_api::reply::{TraceMark, TraceRecord};

/// The frame task, [`crate::net::frames_task`].
pub const FRAMES: usize = 0;
/// The upload loop, [`crate::http`]'s `post_firmware`.
pub const UPLOAD: usize = 1;
/// Any other HTTP request.
pub const REQUEST: usize = 2;
/// Inside [`crate::store::guarded`], interrupts masked: `1` erase or write.
pub const FLASH: usize = 3;
/// The telemetry task, which is what feeds the watchdog when nothing uploads.
pub const TELEMETRY: usize = 4;
const SLOTS: usize = 5;
const NAMES: [&str; SLOTS] = ["frames", "upload", "request", "flash", "telemetry"];

/// This run's marks. `persistent`: zeroed on a power-on reset, kept on every
/// other kind, which is what lets the next boot read them.
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut STEP: [u32; SLOTS] = [0; SLOTS];
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut AT_MS: [u32; SLOTS] = [0; SLOTS];

/// The previous run's marks, steps then times, copied by [`keep_previous`]
/// on every boot before anything of this run can overwrite them.
///
/// **One static, `persistent`, and named to sort last**, all three for the
/// same reason: the update that installs this must find `panic::CRUMB` and
/// the two arrays above at the addresses older builds used, or it reads the
/// old breadcrumb in the wrong place and wipes it. A `zeroed` static is laid
/// out before every persistent one and moves them all; persistent ones are
/// laid out in the order of their mangled names (`5panic5CRUMB`,
/// `5trace4STEP`, `5trace5AT_MS` - checked with `nm` on the card 378 build),
/// and `8PREVIOUS` comes after those. Nothing needs it kept across a reset.
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut PREVIOUS: [u32; 2 * SLOTS] = [0; 2 * SLOTS];

#[inline(always)]
fn read(base: *const u32, i: usize) -> u32 {
    // SAFETY: `i < SLOTS` at every call site, and these are plain words
    // whatever the previous run left.
    unsafe { read_volatile(base.add(i)) }
}

/// `slot` has reached `step`.
#[inline]
pub fn mark(slot: usize, step: u32) {
    let now = Instant::now().as_millis() as u32;
    // SAFETY: word writes to our own statics, on core 0. A torn pair would only
    // be a wrong clue, and each is one aligned word anyway.
    unsafe {
        write_volatile((addr_of_mut!(STEP) as *mut u32).add(slot), step);
        write_volatile((addr_of_mut!(AT_MS) as *mut u32).add(slot), now);
    }
}

/// Copy what the previous run's tasks said last, and say it on the log.
///
/// **First thing in `main`**, before any task or `store` call can `mark`:
/// after that the record is this run's.
pub fn keep_previous() {
    let (steps, times) = (addr_of!(STEP) as *const u32, addr_of!(AT_MS) as *const u32);
    for i in 0..SLOTS {
        // SAFETY: in bounds; core 1 is not running yet and nothing else
        // touches these.
        unsafe {
            write_volatile((addr_of_mut!(PREVIOUS) as *mut u32).add(i), read(steps, i));
            write_volatile((addr_of_mut!(PREVIOUS) as *mut u32).add(SLOTS + i), read(times, i));
        }
    }
    if let Some(t) = previous() {
        for (name, m) in NAMES.iter().zip([t.frames, t.upload, t.request, t.flash, t.telemetry]) {
            info!("trace: previous run: {} at step {}, {} ms before the end", name, m.step, m.before_ms);
        }
    }
}

/// The previous run's record, or `None` when there is none - after a power-on
/// reset, the whole record reads zero.
///
/// `before_ms` is relative to the last mark any task made: the slot furthest
/// behind stopped first.
pub fn previous() -> Option<TraceRecord> {
    let steps = addr_of!(PREVIOUS) as *const u32;
    // SAFETY: the second half of the same array.
    let times = unsafe { steps.add(SLOTS) };
    if (0..SLOTS).all(|i| read(steps, i) == 0 && read(times, i) == 0) {
        return None;
    }
    let last = (0..SLOTS).map(|i| read(times, i)).max().unwrap_or(0);
    let m = |i: usize| TraceMark { step: read(steps, i), before_ms: last.wrapping_sub(read(times, i)) };
    Some(TraceRecord {
        frames: m(FRAMES),
        upload: m(UPLOAD),
        request: m(REQUEST),
        flash: m(FLASH),
        telemetry: m(TELEMETRY),
    })
}
