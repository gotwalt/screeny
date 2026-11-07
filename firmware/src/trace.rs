//! Where core 0's tasks last were, kept where a watchdog reset cannot reach.
//!
//! The liveness watchdog is the right answer to a hung core 0 and the wrong
//! witness: it resets the chip and leaves nothing behind but `last_reset: wdt`.
//! A debugger is not on this bench, and a log line cannot be printed from the
//! inside of a hang. So each suspect writes one small number into RTC slow
//! memory as it goes, and the next boot prints what they said last.
//!
//! A plain volatile write and a timer read, so it is cheap enough for the
//! frame task. Built for chasing the upload hang of 2026-11-07.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use embassy_time::Instant;
use log::info;

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

#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut STEP: [u32; SLOTS] = [0; SLOTS];
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut AT_MS: [u32; SLOTS] = [0; SLOTS];

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

/// Say what the previous run's tasks said last, and when, relative to the last
/// thing any of them said: the one furthest behind stopped first.
pub fn report_previous() {
    let read = |base: *const u32, i: usize| {
        // SAFETY: in bounds, and plain words whatever the previous run left.
        unsafe { read_volatile(base.add(i)) }
    };
    let steps = addr_of!(STEP) as *const u32;
    let times = addr_of!(AT_MS) as *const u32;
    let last = (0..SLOTS).map(|i| read(times, i)).max().unwrap_or(0);
    for i in 0..SLOTS {
        info!(
            "trace: previous run: {} at step {}, {} ms before the end",
            NAMES[i],
            read(steps, i),
            last.wrapping_sub(read(times, i))
        );
    }
}

/// What happened, in words, kept in RAM and served in a comment on the page:
/// serial loses lines under load, so a missing line there proves nothing.
static NOTES: critical_section::Mutex<core::cell::RefCell<heapless::String<NOTES_LEN>>> =
    critical_section::Mutex::new(core::cell::RefCell::new(heapless::String::new()));
const NOTES_LEN: usize = 1024;

/// Add a line, dropping the oldest half when full.
pub fn note(what: core::fmt::Arguments<'_>) {
    use core::fmt::Write;
    let at = Instant::now().as_millis();
    critical_section::with(|cs| {
        let mut s = NOTES.borrow_ref_mut(cs);
        if s.len() > NOTES_LEN - 120 {
            let keep = s.len() / 2;
            let tail: heapless::String<NOTES_LEN> =
                s.char_indices().nth(keep).map(|(i, _)| s[i..].try_into().unwrap_or_default()).unwrap_or_default();
            *s = tail;
        }
        let _ = write!(s, "{at}: {what}; ");
    });
}

/// The notes, written out.
pub fn write_notes(f: &mut impl core::fmt::Write) -> core::fmt::Result {
    critical_section::with(|cs| f.write_str(&NOTES.borrow_ref(cs)))
}
