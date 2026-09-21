//! What happens when this firmware panics: a breadcrumb in RTC memory, then a
//! reset (card 243).
//!
//! Until 0.5.2 a panic ended in `esp-backtrace`'s `abort()`, which is
//! `interrupt_free(|| loop {})`: **core 0 spun forever with interrupts off and
//! core 1 kept the panel lit**, so the device answered nothing, rebooted
//! nothing, and the backtrace went to a UART nobody was attached to. Card 234
//! ranked that as the best single-event explanation of the one silent stall of
//! 0.5.1 and could not settle it, because nothing survived the event. This
//! module is the instrument it asked for, and the behaviour the owner asked for
//! ("pretty crash proof", device-web decision 10): a panic prints, records what
//! and where, and reboots. The device is back on the network in ~15 s and the
//! next boot's log - and `GET /api/v1/status` - say that it panicked.
//!
//! ## The five rules the panic path keeps
//!
//! 1. **Nothing that can block.** No allocation, no lock we take ourselves, no
//!    `.await`, no embassy timer. A panic can arrive from an interrupt with
//!    interrupts already masked, from inside `esp-storage`'s critical section,
//!    or from core 1; a handler that waits for any of those is a handler that
//!    never reaches the reset.
//! 2. **The breadcrumb is written first, printing second.** Printing takes
//!    `esp-println`'s `RawMutex`, which is **not** reentrant (it panics) and is
//!    shared with the other core. If the panic happened inside a `println!`,
//!    or the other core holds that lock and is itself wedged, the printing is
//!    where we die - so the record is already in RTC memory by then.
//! 3. **RTC *slow* memory, not RTC fast.** `#[ram(rtc_fast, ..)]` was the
//!    instrument card 234 proposed and it is wrong for this device: ESP32
//!    RTC_FAST is reachable from the PRO CPU only (esp-hal's own
//!    `ld/esp32/memory.x`: "Only for core 0"), and a panic on core 1 - the
//!    display core - must leave the same breadcrumb. RTC_SLOW (8 KB at
//!    0x5000_0000) is reachable from both. Either way it costs **zero bytes of
//!    `.bss`**, which is the pool that breaks first on this chip.
//! 4. **A crash loop must end.** [`CRASH_LOOP_MAX`] quick panics in a row and
//!    the device stops resetting: the next boot shows a "crashed" screen and
//!    goes no further. A panel that reboots forever on USB power is worse than
//!    one that says it is broken.
//! 5. **Never a second opinion about the PSK.** Nothing here formats anything
//!    but a source file name, a line number and a few counters.
//!
//! ## Why this file owns `#[panic_handler]` instead of `esp-backtrace`
//!
//! `esp-backtrace`'s `custom-halt` hook is `fn custom_halt() -> !` - it takes
//! no arguments, so the `&PanicInfo` (and with it the `file:line` this card is
//! asked to record) is already gone by the time it runs. The backtrace itself
//! is still `esp-backtrace`'s: [`Backtrace::capture`] is its public API and is
//! not behind the `panic-handler` feature, so what goes to the serial port is
//! the same banner, the same `PanicInfo` line and the same `0x4...` frame list
//! an `espflash monitor` already knows how to symbolise. What we gain is the
//! order (rule 2), the nesting guard, and the location.

use core::sync::atomic::{AtomicU32, Ordering};

use esp_println::println;
use log::{info, warn};

// ---------------------------------------------------------------------------
// The crash-loop guard's two numbers
// ---------------------------------------------------------------------------

/// Consecutive quick panics that stop the device resetting.
///
/// **Five, each under [`QUICK_MS`].** Four is a guess about how many times a
/// transient deserves a retry; five is the card's own suggestion and is the
/// number where the two failures being traded off cross. A device that panics
/// on something transient - a beacon that upsets the radio, an AP that answers
/// one DHCP offer badly - has to heal itself without anybody in the room, and
/// each attempt costs one boot. Five quick attempts is at most ~2 minutes of
/// flapping, which is short enough that an owner watching the panel sees it
/// settle or sees it stop, and long enough that a one-in-five transient is
/// survived.
pub const CRASH_LOOP_MAX: u32 = 5;

/// What counts as "it panicked again straight away".
///
/// A panic later than this resets the run to one, so a device that panics once
/// a day never reaches the guard. 60 s is deliberately the same threshold
/// research 006 section 6 gives the OTA health criterion ("never before 60 s"):
/// the two questions - "did this image survive?" and "is this a crash loop?" -
/// should not disagree about what an early death is.
pub const QUICK_MS: u32 = 60_000;

/// How long the panic path waits before resetting the chip.
///
/// Two things are being waited for and the longer one sets the number.
/// The UART: `esp-println` on this chip is the ROM's `uart_tx_one_char`, which
/// waits for FIFO *space* and not for the line to drain, so up to 128 bytes can
/// still be in flight - 11 ms at 115200 baud. The flash: a panic can land
/// inside `esp-storage`'s 4 KB sector erase (research 006 section 4 measures
/// ~50 ms), which the chip finishes by itself whatever the CPU does - and the
/// ROM bootloader's first act after the reset is to read that same flash. 120 ms
/// covers both with room, and it is time spent by a device that has already
/// failed.
const SETTLE_MS: u32 = 120;

/// CPU cycles in [`SETTLE_MS`] at the 240 MHz this firmware configures.
///
/// A cycle count rather than `Instant::now()` on purpose: `CCOUNT` runs from
/// reset and needs no peripheral to have been initialised, so this is still a
/// bounded wait for a panic that arrives before `esp_hal::init` has set the
/// timer up. A slower clock only makes the wait longer, which is harmless.
const SETTLE_CYCLES: u32 = 240_000 * SETTLE_MS;

// ---------------------------------------------------------------------------
// The breadcrumb
// ---------------------------------------------------------------------------

/// `"SCB"` and a layout version. Anything else in word 0 means the region has
/// never been written by this firmware (or has been written by another one).
/// Bumped to `02` by card 241b, which added [`W_SECTOR`]. An older record is
/// then simply "not ours" and is wiped, which loses one boot counter across the
/// update that installs this and nothing else.
const MAGIC: u32 = 0x5343_4202;

/// Mixed into the checksum so that an all-zero or all-ones region cannot pass.
const CHECK_SALT: u32 = 0x9e37_79b9;

/// A panic record is present (words [`W_UPTIME`]..=[`W_PANIC_BOOT`] mean
/// something).
const F_PANIC: u32 = 1 << 0;
/// The crash-loop guard has latched: this boot must not start the device.
const F_HALT: u32 = 1 << 1;
/// The `panic-test` build has already fired since the last power-on.
///
/// The bit is declared in every build although only that one reads it: the
/// layout of the breadcrumb must not depend on a cargo feature, or a bench
/// build and a default build would disagree about a region that survives the
/// reflash between them.
#[cfg_attr(not(feature = "panic-test"), allow(dead_code))]
const F_TEST_FIRED: u32 = 1 << 2;
/// **A firmware upload was in flight when the chip last reset** (card 241b).
///
/// Card 241 used this bit to say "the next boot is a trial", so that the RTC
/// watchdog could be armed before flash was up. The watchdog is always on now
/// (`ota::arm_liveness_watchdog`), so nothing has to be remembered for that -
/// and the bit is worth more spent on the question the bench actually had to
/// ask: **the device went quiet, and was it in the middle of an upload?**
///
/// Set by `ota::Upload::start`, cleared by its `Drop`, and reported by
/// [`boot`] on the next boot. It is one bit in a word that is already written,
/// so it costs nothing, and it is exactly the fact a serial log nobody was
/// attached to would otherwise have taken with it.
const F_IN_UPLOAD: u32 = 1 << 3;

const W_MAGIC: usize = 0;
/// Boots since the last power-on, including this one.
const W_BOOTS: usize = 1;
/// Panics since the last power-on.
const W_PANICS: usize = 2;
/// Quick panics in a row, as of the last one.
const W_RUN: usize = 3;
/// Uptime in ms at the last panic.
const W_UPTIME: usize = 4;
/// Source line of the last panic.
const W_LINE: usize = 5;
/// Twelve bytes of the source file's base name, NUL padded.
const W_FILE: usize = 6;
/// Boot number that panicked.
const W_PANIC_BOOT: usize = 9;
/// `F_*`.
const W_FLAGS: usize = 10;
/// The reset reason the *previous* boot started with.
const W_PREV_RESET: usize = 11;
/// **How far a firmware upload had got, in sectors** (card 241b).
///
/// Meaningful only with [`F_IN_UPLOAD`] set. The one word this card added, and
/// it earns its place: fw 0.7.0 wedged "about fifteen seconds" into staging on
/// three runs out of three, and nobody could say whether that was a *time* or
/// an *offset* - which is the difference between a race and a bad address.
/// With this, the boot after a reset answers it without anyone guessing.
const W_SECTOR: usize = 12;
/// XOR of every word above, plus [`CHECK_SALT`].
const W_CHECK: usize = 13;
const WORDS: usize = 14;

/// 52 bytes of RTC slow memory, and **not one byte of `.bss`**.
///
/// `persistent` means the runtime zeroes it on a power-on reset and leaves it
/// alone on every other kind (`esp-hal`'s `__init_persistent`). That is exactly
/// the lifetime this wants: the counters are "since the device last lost
/// power", a panic-reset preserves them, and pulling the plug is the documented
/// way to clear a latched crash-loop halt.
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut CRUMB: [u32; WORDS] = [0; WORDS];

#[inline(always)]
fn get(i: usize) -> u32 {
    // SAFETY: `i < WORDS` at every call site (they are the `W_*` constants),
    // and a `u32` read from RTC memory is atomic on this chip. Volatile
    // because the compiler must not cache or reorder these across the reset.
    unsafe { core::ptr::read_volatile((&raw const CRUMB).cast::<u32>().add(i)) }
}

#[inline(always)]
fn put(i: usize, v: u32) {
    // SAFETY: see [`get`].
    unsafe { core::ptr::write_volatile((&raw mut CRUMB).cast::<u32>().add(i), v) }
}

fn checksum() -> u32 {
    let mut x = CHECK_SALT;
    for i in 0..W_CHECK {
        x ^= get(i);
    }
    x
}

/// Stamp the checksum. **Always the last write of any update**, so that a reset
/// landing in the middle of one is read back as "invalid" rather than as a
/// half-updated record.
fn seal() {
    put(W_CHECK, checksum());
}

fn intact() -> bool {
    get(W_MAGIC) == MAGIC && get(W_CHECK) == checksum()
}

fn wipe() {
    for i in 0..WORDS {
        put(i, 0);
    }
    put(W_MAGIC, MAGIC);
    seal();
}

// ---------------------------------------------------------------------------
// What the rest of the firmware reads
// ---------------------------------------------------------------------------

/// The last panic, as the status API and the boot log report it.
#[derive(Clone, Copy)]
pub struct Panic {
    /// Uptime in milliseconds when it happened.
    pub uptime_ms: u32,
    /// Which boot panicked (`1` is the first boot after power-on).
    pub boot: u32,
    /// Source line.
    pub line: u32,
    /// Quick panics in a row including this one.
    pub consecutive: u32,
    /// The source file's base name, NUL padded. Use [`Panic::file`].
    file: [u8; 12],
}

impl Panic {
    /// The source file's base name, e.g. `net.rs`. Never empty: an
    /// unattributed panic reads `?`.
    #[must_use]
    pub fn file(&self) -> &str {
        let n = self.file.iter().position(|b| *b == 0).unwrap_or(self.file.len());
        match core::str::from_utf8(&self.file[..n]) {
            Ok("") | Err(_) => "?",
            Ok(s) => s,
        }
    }
}

/// Everything the breadcrumb says, read at boot and while the device runs.
#[derive(Clone, Copy)]
pub struct Report {
    /// Boots since the last power-on, including this one.
    pub boots: u32,
    /// Panics since the last power-on.
    pub panics: u32,
    /// The last one, if there has been one.
    pub last: Option<Panic>,
    /// The crash-loop guard has latched: do not start the device.
    pub halt: bool,
}

/// Read the breadcrumb. Cheap - a dozen volatile loads - so callers do not
/// cache it, and `GET /api/v1/status` reads it per request.
#[must_use]
pub fn report() -> Report {
    if !intact() {
        return Report {
            boots: 0,
            panics: 0,
            last: None,
            halt: false,
        };
    }
    let flags = get(W_FLAGS);
    let last = (flags & F_PANIC != 0).then(|| {
        let mut file = [0u8; 12];
        for (w, chunk) in file.chunks_mut(4).enumerate() {
            chunk.copy_from_slice(&get(W_FILE + w).to_le_bytes());
        }
        Panic {
            uptime_ms: get(W_UPTIME),
            boot: get(W_PANIC_BOOT),
            line: get(W_LINE),
            consecutive: get(W_RUN),
            file,
        }
    });
    Report {
        boots: get(W_BOOTS),
        panics: get(W_PANICS),
        last,
        halt: flags & F_HALT != 0,
    }
}

/// Count this boot, remember why it happened, and say what the last one left
/// behind.
///
/// Call it from `main` as soon as the logger is up and **before** anything that
/// can panic, so that a boot-path panic is still counted against the crash-loop
/// guard.
pub fn boot() -> bool {
    // **A short wait and a blank line before the first word this firmware
    // says.** The bench sees the *first* log line come out partly garbled on
    // every boot while the second is clean: the ROM logs at 74880 baud and the
    // second-stage bootloader at 115200, so when `main` starts there are still
    // bytes in flight from a different bit rate and the monitor is midway
    // through a character. Twenty milliseconds is three characters at the
    // slowest of those rates, and the newline gives whatever the monitor has
    // half-decoded a line of its own to die on. It costs a boot 20 ms, once.
    xtensa_lx::timer::delay(240_000 * 20);
    println!();
    let reason = raw_reset_reason();
    if !intact() {
        // Either the first boot after a power-on (the runtime zeroed the
        // region) or a region that is not ours. Both start from scratch.
        wipe();
    }
    let prev = get(W_PREV_RESET);
    put(W_BOOTS, get(W_BOOTS).saturating_add(1));
    put(W_PREV_RESET, reason);
    seal();

    let r = report();
    info!(
        "boot: #{} since power-on, reset reason {} (the boot before it started with {})",
        r.boots,
        reason_word(reason),
        if r.boots > 1 { reason_word(prev) } else { "-" },
    );
    match r.last {
        None => info!("boot: no panic on record"),
        Some(p) => warn!(
            "boot: last panic was boot #{} at uptime {} ms, {}:{}, {} in a row ({} panic(s) since power-on)",
            p.boot,
            p.uptime_ms,
            p.file(),
            p.line,
            p.consecutive,
            r.panics,
        ),
    }
    // Card 241b: **the line that would have saved an evening.** If the bit is
    // still set, the boot before this one was reset while a firmware upload
    // was in flight, and this boot is the watchdog (or a brownout, or a panic)
    // having ended it. Cleared here, so it means the *last* reset and never an
    // older one.
    if get(W_FLAGS) & F_IN_UPLOAD != 0 {
        let sectors = get(W_SECTOR);
        warn!(
            "boot: the previous boot was reset while a FIRMWARE UPLOAD was in flight - reset reason {}, after {} sector(s), i.e. {} bytes in (offset {:#x} of the staged slot). otadata is untouched, so this is the image that was running before.",
            reason_word(reason),
            sectors,
            sectors * 4096,
            sectors * 4096,
        );
        put(W_FLAGS, get(W_FLAGS) & !F_IN_UPLOAD);
        seal();
    }
    if r.halt {
        warn!(
            "boot: CRASH LOOP - {} panics in a row, each within {} s of a boot. Not starting: the panel says so. Power-cycle the device to clear the breadcrumb.",
            CRASH_LOOP_MAX,
            QUICK_MS / 1000,
        );
    }
    // **A `bool`, not the `Report`.** `main` is an embassy task, so anything it
    // holds across an `await` is part of a future in `.bss`, and `.bss` is core
    // 0's stack: carrying the whole report from here to the crash-loop check
    // cost 128 bytes of ceiling for a value that cannot change. The check reads
    // it again with [`report`], which is a dozen volatile loads.
    r.halt
}

// ---------------------------------------------------------------------------
// The one bit card 241b keeps here
// ---------------------------------------------------------------------------

/// Note that a firmware upload is in flight, or that it has finished.
///
/// Called by `ota::Upload` from both ends. See [`F_IN_UPLOAD`]: it is what
/// makes the boot after a silent wedge say *what the device was doing*, which
/// is the fact card 241b spent an evening not having.
pub fn set_in_upload(busy: bool) {
    if !intact() {
        wipe();
    }
    let flags = get(W_FLAGS);
    let next = if busy {
        flags | F_IN_UPLOAD
    } else {
        flags & !F_IN_UPLOAD
    };
    if next != flags {
        put(W_SECTOR, 0);
        put(W_FLAGS, next);
        seal();
    }
}

/// How many sectors of the staged image have been written, so far.
///
/// Called once per sector from the staging loop. Two volatile writes and a
/// fourteen-word XOR against a sector that costs a hundred milliseconds, so it
/// is free; and if the chip resets mid-upload it is the difference between
/// "about fifteen seconds in" and "at sector 147, offset 0x93000".
pub fn note_upload_sector(sectors: u32) {
    if !intact() {
        return;
    }
    put(W_SECTOR, sectors);
    seal();
}

/// The reset reason this boot started with, as [`crate::screeny_device_api`]'s
/// [`ResetReason`] spells it.
///
/// The same register [`crate::http`] reads, exposed here so that
/// `GET /api/v1/panic` can carry it beside the boot counter: card 241b's
/// question is "did this device reset itself, and why", and a reader of the
/// breadcrumb route should not have to poll `status` to find out.
///
/// [`ResetReason`]: screeny_device_api::ResetReason
#[must_use]
pub fn reset_reason() -> screeny_device_api::ResetReason {
    use screeny_device_api::ResetReason as R;
    match get(W_PREV_RESET) {
        1 => R::PowerOn,
        2 | 3 => R::Software,
        4 => R::DeepSleep,
        5 => R::Sdio,
        6 => R::Brownout,
        7 | 8 => R::Wdt,
        _ => R::Unknown,
    }
}

/// The `panic-test` build's one-shot latch: `true` the first time it is called
/// after a power-on, `false` ever after.
///
/// Reading and setting it is one call on purpose - the test must be armed
/// *before* it panics, and a two-call shape is a way to arm it after.
#[cfg(feature = "panic-test")]
#[must_use]
pub fn take_test_shot() -> bool {
    if !intact() {
        wipe();
    }
    let flags = get(W_FLAGS);
    if flags & F_TEST_FIRED != 0 {
        return false;
    }
    put(W_FLAGS, flags | F_TEST_FIRED);
    seal();
    true
}

// ---------------------------------------------------------------------------
// The panic path
// ---------------------------------------------------------------------------

/// Which core is in the handler, plus one, or 0 for "nobody".
///
/// Four bytes of `.data`, not of the breadcrumb: it must **not** survive the
/// reset, and it is only ever read on the way into a handler.
static IN_PANIC: AtomicU32 = AtomicU32::new(0);

/// What the handler does when it has finished printing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    Reset,
    /// The crash-loop guard had already latched before this panic: the boot
    /// that was supposed to be showing the crashed screen has panicked too.
    Halt,
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let me = match esp_hal::system::Cpu::current() {
        esp_hal::system::Cpu::ProCpu => 1,
        _ => 2,
    };
    let first = IN_PANIC.swap(me, Ordering::Relaxed) == 0;

    // Rule 2: the record, before anything that can block. A nested panic adds
    // nothing to it - the first one is the one worth keeping - and goes
    // straight for the reset.
    let action = if first {
        record(info.location())
    } else {
        After::Reset
    };

    if first {
        // What `esp-backtrace`'s own handler prints, in its order, so that
        // `espflash monitor` symbolises the frames exactly as before.
        println!("");
        println!("====================== PANIC ======================");
        println!("{}", info);
        println!("");
        println!("Backtrace:");
        println!("");
        for frame in esp_backtrace::Backtrace::capture().frames() {
            println!("0x{:x}", frame.program_counter());
        }
        println!("");
    }

    match action {
        After::Halt => {
            // The boot that was meant to be showing the crashed screen has
            // panicked too. Resetting again would be the loop the guard exists
            // to stop, so this is the one path that still ends the way every
            // panic ended before 0.5.2 - deliberately, and having said so.
            println!(
                "panic: the crash-loop guard has latched and this boot panicked as well; halting instead of resetting. Power-cycle to clear."
            );
            settle();
            halt()
        }
        After::Reset => {
            if first {
                println!("panic: resetting the chip (card 243; the breadcrumb is in RTC memory).");
            }
            // A nested panic - the same core panicking inside the printing
            // above, or the other core arriving here while the first is still
            // working - prints nothing at all (printing is where a deadlock
            // would be) and spends [`SETTLE_MS`] waiting for the first one's
            // reset before forcing its own.
            settle();
            esp_hal::system::software_reset()
        }
    }
}

/// Write the panic into the breadcrumb, and say what to do next.
///
/// Every value here is read and written directly; nothing in this function can
/// suspend, allocate, take a lock or fail.
fn record(loc: Option<&core::panic::Location<'_>>) -> After {
    if !intact() {
        wipe();
    }
    // Read before the update: the guard latches on the panic that reaches
    // [`CRASH_LOOP_MAX`] and that panic still resets, so that the next boot can
    // put the crashed screen up. Only a panic that finds the latch *already*
    // set has proved the screen itself cannot be reached.
    let latched = get(W_FLAGS) & F_HALT != 0;
    let uptime = uptime_ms();
    let run = if uptime < QUICK_MS {
        get(W_RUN).saturating_add(1)
    } else {
        1
    };
    put(W_PANICS, get(W_PANICS).saturating_add(1));
    put(W_RUN, run);
    put(W_UPTIME, uptime);
    put(W_PANIC_BOOT, get(W_BOOTS));
    put(W_LINE, loc.map_or(0, core::panic::Location::line));
    let name = loc.map_or("?", |l| base_name(l.file()));
    for w in 0..3 {
        let mut bytes = [0u8; 4];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = name.as_bytes().get(w * 4 + i).copied().map_or(0, keep);
        }
        put(W_FILE + w, u32::from_le_bytes(bytes));
    }
    let mut flags = get(W_FLAGS) | F_PANIC;
    if run >= CRASH_LOOP_MAX {
        flags |= F_HALT;
    }
    put(W_FLAGS, flags);
    seal();
    if latched { After::Halt } else { After::Reset }
}

/// The last 12 bytes of a source path's base name.
///
/// The base name and not the path, because a path has `/` in it and this string
/// goes into JSON: picoserve escapes `/` as `\/` where the other two writers do
/// not, and the API's golden files are the one form all three agree on. The
/// *last* 12 bytes rather than the first so that the extension survives, which
/// is what tells `mutex.rs` from `mutex`.
fn base_name(path: &str) -> &str {
    let name = match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    };
    let b = name.as_bytes();
    if b.len() <= 12 {
        return name;
    }
    // Bytes, not characters: everything below `keep` is ASCII anyway, and a
    // cut that lands mid-character is repaired there.
    core::str::from_utf8(&b[b.len() - 12..]).unwrap_or("?")
}

/// Keep the bytes a file name is made of and nothing else.
///
/// A panic can come from any crate in the tree, so this is what stops a strange
/// path from putting a quote, a backslash or a control character into a JSON
/// reply, and what keeps [`Panic::file`] valid UTF-8 whatever was stored.
const fn keep(b: u8) -> u8 {
    match b {
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-' => b,
        _ => b'?',
    }
}

/// Milliseconds since reset, without needing anything but the timer group.
///
/// `esp_hal::time::Instant::now()` and not `embassy_time`: the embassy clock is
/// a driver with state, this is two register reads, and the panic may have come
/// from a context where the executor is the thing that is broken.
fn uptime_ms() -> u32 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_millis() as u32
}

/// Let the UART drain and any in-flight flash erase finish. See [`SETTLE_MS`].
fn settle() {
    xtensa_lx::timer::delay(SETTLE_CYCLES);
}

/// Stop, with interrupts off, for ever. The pre-0.5.2 behaviour, now reached
/// only by the crash-loop guard's last line.
fn halt() -> ! {
    // Empty on purpose: there is nothing left to do and nothing that may run.
    // `esp-backtrace`'s own `abort()` spells it the same way.
    #[allow(clippy::empty_loop)]
    xtensa_lx::interrupt::free(|| loop {})
}

/// Why the chip last restarted, as a code of this module's own.
///
/// Not `SocResetReason as u32`: that discriminant is a hardware number from a
/// crate we pin, and the breadcrumb outlives a firmware update (card 241 will
/// read it across one). A small closed set written here is a number whose
/// meaning cannot change under a dependency bump. It is also finer than
/// `crates/device-api`'s [`ResetReason`], which folds both software resets
/// together, because "which core asked for it" is exactly the sort of thing the
/// next stall will want.
///
/// [`ResetReason`]: screeny_device_api::ResetReason
fn raw_reset_reason() -> u32 {
    use esp_hal::rtc_cntl::SocResetReason as R;
    match esp_hal::rtc_cntl::reset_reason(esp_hal::system::Cpu::ProCpu) {
        Some(R::ChipPowerOn) => 1,
        Some(R::CoreSw) => 2,
        Some(R::Cpu0Sw) => 3,
        Some(R::CoreDeepSleep) => 4,
        Some(R::CoreSdio) => 5,
        Some(R::SysBrownOut) => 6,
        Some(R::CoreMwdt0 | R::CoreMwdt1 | R::CpuMwdt0) => 7,
        Some(R::CoreRtcWdt | R::Cpu0RtcWdt | R::SysRtcWdt) => 8,
        Some(_) => 9,
        None => 0,
    }
}

/// The same code as a word, for the boot line only.
fn reason_word(raw: u32) -> &'static str {
    match raw {
        1 => "power_on",
        2 => "software",
        3 => "cpu0_software",
        4 => "deep_sleep",
        5 => "sdio",
        6 => "brownout",
        7 => "timer_wdt",
        8 => "rtc_wdt",
        9 => "other",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// The bench build that proves it (feature `panic-test`)
// ---------------------------------------------------------------------------

/// Panic on core 0, once per power-on, some seconds after boot.
///
/// The orchestrator's proof that the whole path works end to end: backtrace,
/// breadcrumb, reset, rejoin, and the next boot reporting it over HTTP. It
/// fires **once** - the latch is in the breadcrumb, so the reset it causes does
/// not arm it again - which is what keeps a bench build from becoming the crash
/// loop this card is about.
///
/// Twenty seconds: long enough that the device has joined (a boot-to-join is
/// ~15 s) and the Studio has seen it, short enough that the whole cycle fits in
/// a one-minute watch.
#[cfg(feature = "panic-test")]
#[embassy_executor::task]
pub async fn panic_test_task() {
    use embassy_time::{Duration, Timer};

    const AFTER_S: u64 = 20;

    if !take_test_shot() {
        info!("panic-test: already fired since power-on; this boot runs normally");
        return;
    }
    warn!(
        "panic-test: BENCH BUILD - panicking on core 0 in {} s, once (card 243)",
        AFTER_S
    );
    Timer::after(Duration::from_secs(AFTER_S)).await;
    panic!("panic-test: deliberate panic on core 0 (card 243)");
}
