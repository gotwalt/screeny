//! The button on the back of the device (card 230).
//!
//! One task, one pin, and the gesture rules in a crate that has host tests:
//! [`screeny_provision::button`] turns levels and milliseconds into
//! [`ButtonEvent`]s and this module is the half it deliberately does not have -
//! a clock, a pin, a panel and a state machine to tell.
//!
//! ```text
//!   GPIO15 ──▶ Recognizer ──▶ ShortPress ──▶ IDENTIFY for 10 s (crate::http)
//!                         ├─▶ HoldStarted/Tick ──▶ the countdown screen
//!                         ├─▶ HoldCancelled ──▶ "cancelled", one second
//!                         ├─▶ HoldRefused ──▶ "not while updating"
//!                         └─▶ WipeWifi ──▶ Event::ButtonWipe (crate::provision)
//! ```
//!
//! ## The pin, and why it is configured the way it is
//!
//! **GPIO15, input, internal pull-up, active low.** That is what the stock
//! firmware does, byte for byte (`docs/research/008-button.md` section 1), and
//! card 203 confirmed it on this unit with the owner pressing: edges on GPIO15
//! and on no other candidate, high at rest, low while held, one 11 ms bounce
//! in eleven presses.
//!
//! 008's point 4 is the part that has to be read before touching this pin:
//! **GPIO15 is dual-purpose on this board.** It is also `BOARD_ID_ADC_B`
//! ([`crate::tidbyt::pins`]) - ADC2 channel 3, which the newer stock build
//! samples for the hardware revision - and it is the MTDO strapping pin. Three
//! consequences, all of them live in this file:
//!
//! 1. **Nothing here ever ADC-reads GPIO15, and nothing else may while this
//!    task is running.** The two uses are exclusive. If board-revision
//!    detection is ever added it reads the strap *once, before* this task is
//!    spawned, and it must tolerate the button being held (which reads ~0 mV).
//! 2. **The internal pull-up is belt and braces, not the only thing holding
//!    the pin up.** Card 203's phase B settled that: with the internal
//!    *pull-down* selected the pin still rested high, so the strap network
//!    holds it up and the button pulls it firmly to ground against that. The
//!    pull-up is configured anyway, because the stock firmware does and because
//!    a build that ever met a board without the strap would otherwise float.
//! 3. **A held button at boot silences the ROM boot log.** GPIO15 is MTDO: the
//!    ESP32 samples it at reset and a low level suppresses the ROM's first
//!    lines. It is harmless and it is not this firmware's doing - but somebody
//!    will one day spend an hour on "the serial port went quiet when I held the
//!    button while plugging it in", so it is written down here.
//!
//! The pin is **not** GPIO0, so holding it at boot cannot strand the device in
//! the ROM bootloader (008 point 3).
//!
//! ## What it costs the frame path
//!
//! Device-web decision 7: the frame path is the product. The task spends its
//! life in `Input::wait_for_any_edge()`, which is interrupt-backed, raced
//! against a timer - [`IDLE_POLL`] when nothing is happening, [`ACTIVE_POLL`]
//! while a gesture is in flight. An untouched button is one wake a second; a
//! held one is twenty, which is what the countdown's one-second ticks are made
//! of. Nothing spins, nothing is held across an `await` but the recogniser
//! (32 bytes), and no flash is touched here at all: the wipe is a signal to
//! the provisioning task, which owns every write.

use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{AnyPin, Input, InputConfig, Pull};
use log::{info, warn};
use screeny_provision::button::{ButtonEvent, Level, Recognizer, STATUS_MS};

/// How often the pin is read while nothing is happening.
///
/// The edge interrupt is what actually notices a press (esp-hal binds a
/// default GPIO handler in `esp_hal::init`, which is what makes
/// `wait_for_any_edge` work without any setup here); this is the backstop that
/// keeps a missed edge from turning into a button that has stopped working
/// until the next reboot.
const IDLE_POLL: Duration = Duration::from_secs(1);

/// How often the pin is read while a gesture is in flight. Fine enough that
/// the 1 s and 5 s thresholds land within a frame time of where they belong,
/// and it is only ever for the few seconds somebody is holding the button.
const ACTIVE_POLL: Duration = Duration::from_millis(50);

/// How long "cancelled" stays up after a hold is let go.
const CANCELLED_MS: u32 = 1_200;

/// How long the "not while updating" refusal stays up.
const REFUSED_MS: u32 = 2_500;

// The published screen, for the frame task. Two atomics and a deadline rather
// than a mutex: `crate::net::frames_task` reads this on every 20 ms tick and
// must never wait for anything, least of all for a task that is in the middle
// of a flash write.
const SHOW_NONE: u8 = 0;
const SHOW_COUNTDOWN: u8 = 1;
const SHOW_CANCELLED: u8 = 2;
const SHOW_REFUSED: u8 = 3;

static SHOW: AtomicU8 = AtomicU8::new(SHOW_NONE);
static SECONDS_LEFT: AtomicU8 = AtomicU8::new(0);
/// When the transient screens stop being shown. Only meaningful for
/// [`SHOW_CANCELLED`] and [`SHOW_REFUSED`]; the countdown ends when the button
/// is let go, not on a clock.
static UNTIL_MS: AtomicU32 = AtomicU32::new(0);

/// What the panel should be showing on account of the button, if anything.
///
/// The same shape as [`crate::ota::panel`], and for the same reason: one
/// question for the frame task instead of three atomics it would have to
/// interpret.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// A wipe is `seconds_left` away and letting go cancels it.
    Countdown(u8),
    /// A hold was let go in time; nothing was changed.
    Cancelled,
    /// A hold was refused: an update is in flight or on trial.
    Refused,
}

/// The screen the button wants, if it wants one.
#[must_use]
pub fn panel() -> Option<Panel> {
    match SHOW.load(Ordering::Relaxed) {
        SHOW_COUNTDOWN => Some(Panel::Countdown(SECONDS_LEFT.load(Ordering::Relaxed))),
        // The task clears these itself; the deadline is here so that a task
        // that is somehow not running cannot leave a screen on the panel for
        // ever. Wrapping-safe: the difference is compared as a signed value.
        s @ (SHOW_CANCELLED | SHOW_REFUSED) => {
            let left = UNTIL_MS
                .load(Ordering::Relaxed)
                .wrapping_sub(crate::now_ms()) as i32;
            if left <= 0 {
                return None;
            }
            Some(if s == SHOW_CANCELLED {
                Panel::Cancelled
            } else {
                Panel::Refused
            })
        }
        _ => None,
    }
}

fn show_countdown(seconds_left: u8) {
    SECONDS_LEFT.store(seconds_left, Ordering::Relaxed);
    SHOW.store(SHOW_COUNTDOWN, Ordering::Relaxed);
}

fn show_for(what: u8, ms: u32) {
    UNTIL_MS.store(crate::now_ms().wrapping_add(ms), Ordering::Relaxed);
    SHOW.store(what, Ordering::Relaxed);
}

fn clear() {
    SHOW.store(SHOW_NONE, Ordering::Relaxed);
}

/// May the button forget the network right now?
///
/// **No, while a firmware image is being uploaded, is waiting to be activated,
/// or is on trial and has not confirmed itself** (card 230's open question).
/// An upload would be orphaned: it is streaming over the LAN this wipe is
/// about to drop, and the device would come up in the portal with half an
/// image in the inactive slot. A trial is worse - the health check needs an
/// address and a served request to confirm the running image, so wiping the
/// credentials mid-trial is a reliable way to get the firmware the owner just
/// installed rolled back at 180 s.
///
/// The refusal is visible ([`Panel::Refused`]), it is logged, and it is over
/// in at most three and a half minutes, which is the longest a trial lasts.
/// During an *upload* the panel is already saying "updating - do not unplug",
/// which is the same answer in more useful words, so the refusal screen yields
/// to it in `crate::net`.
fn wipe_allowed() -> bool {
    !crate::ota::updating() && !crate::ota::activating() && !crate::ota::trial_pending()
}

/// Owns GPIO15 and nothing else. Never returns.
#[embassy_executor::task]
pub async fn button_task(pin: AnyPin<'static>) {
    // See the module docs for every word of this line.
    let mut input = Input::new(pin, InputConfig::default().with_pull(Pull::Up));
    let mut rec = Recognizer::new();
    info!(
        "button: gpio{} input, pull-up, active low ({} ms debounce, {} ms hold, {} ms wipe)",
        crate::tidbyt::BUTTON_GPIO.unwrap_or(0),
        screeny_provision::button::DEBOUNCE_MS,
        screeny_provision::button::HOLD_MS,
        screeny_provision::button::WIPE_MS,
    );
    if input.is_low() {
        warn!("button: held at boot - ignored until it is released (it is also MTDO, so the ROM boot log above may be missing)");
    }

    loop {
        let wait = if rec.active() { ACTIVE_POLL } else { IDLE_POLL };
        // `wait_for_any_edge` is not cancellation-safe - dropping it waits for
        // the *next* edge - and it does not need to be: the level is read
        // below on every pass, so a notification lost to the timer costs
        // nothing but the timer's own latency.
        select(input.wait_for_any_edge(), Timer::after(wait)).await;

        let ev = rec.poll(
            Level::from_is_low(input.is_low()),
            crate::now_ms(),
            wipe_allowed(),
        );
        match ev {
            ButtonEvent::None => {}
            ButtonEvent::ShortPress => {
                info!("button: short press - the status screen for {} s", STATUS_MS / 1_000);
                identify().await;
            }
            ButtonEvent::HoldStarted { seconds_left } | ButtonEvent::HoldTick { seconds_left } => {
                if matches!(ev, ButtonEvent::HoldStarted { .. }) {
                    warn!("button: held - wiping the wifi credentials in {} s unless it is let go", seconds_left);
                }
                show_countdown(seconds_left);
            }
            ButtonEvent::HoldCancelled => {
                info!("button: let go - nothing changed");
                show_for(SHOW_CANCELLED, CANCELLED_MS);
            }
            ButtonEvent::HoldRefused => {
                warn!("button: the wifi reset is refused while a firmware update is in flight or on trial");
                show_for(SHOW_REFUSED, REFUSED_MS);
            }
            ButtonEvent::WipeWifi => {
                warn!("button: held {} s - forgetting the wifi credentials and raising the setup portal", screeny_provision::button::WIPE_MS / 1_000);
                clear();
                crate::provision::BUTTON_WIPE.signal(());
            }
            // `ButtonEvent` is `#[non_exhaustive]`.
            _ => {}
        }

        // Let the transient screens expire without waiting for the next edge:
        // `panel()` already refuses to report them past their deadline, and
        // this is what stops `SHOW` from being left set.
        if matches!(SHOW.load(Ordering::Relaxed), SHOW_CANCELLED | SHOW_REFUSED)
            && panel().is_none()
        {
            clear();
        }
    }
}

/// Where the button's synthetic control datagram claims to come from.
///
/// The same trick `crate::http` plays (`FROM_HTTP`) and for the same reason:
/// the address only reaches the control core's log events and its `GET_INFO`
/// rate limiter, and `IDENTIFY` does not touch the frame-source lock, so a
/// fixed local address cannot take the panel away from a streaming sender.
/// Port 0 is not a port anything can send from, which is what makes it
/// readable in a log as "this one came from the button".
const FROM_BUTTON: embassy_net::IpEndpoint =
    embassy_net::IpEndpoint::new(embassy_net::IpAddress::v4(127, 0, 0, 1), 0);

/// The short press, through the mechanism that already exists.
///
/// `IDENTIFY` (spec 6.3) is exactly this screen: "which one is this?", over
/// whatever else is on the panel, with the stream still decoded underneath. So
/// a short press is an `IDENTIFY` of [`STATUS_MS`] fed to the one receive core
/// as a `req_id` 0 request - section 6.1's "no reply wanted" - rather than a
/// second overlay timer beside it. Pressing again during the ten seconds
/// restarts them, which is what anybody pressing a button twice means.
///
/// Written out here rather than calling `crate::http::apply_control`, which
/// does exactly this and then awaits a flash write for the opcodes that need
/// one. Awaiting that future from this task would put the whole settings-write
/// call chain into this task's `.bss` - which is core 0's stack - for an
/// opcode that never writes anything. It cost 600 bytes when it was written
/// the other way, and `docs/design/device-web.md` is explicit: nothing large
/// across an `await`.
async fn identify() {
    let duration_ms = STATUS_MS.min(u16::MAX as u32) as u16;
    // An `IDENTIFY` request is ten bytes: an 8-byte control header and a
    // `u16le`. Nothing is written back, because `req_id` is 0.
    let mut buf = [0u8; 16];
    let mut reply = [0u8; 16];
    let Ok(n) = screeny_proto::control::Request::Identify { duration_ms }.write(0, &mut buf) else {
        warn!("button: the identify request would not encode");
        return;
    };
    let mut imm = None;
    let mut guard = crate::net::CORE.lock().await;
    let Some(core) = guard.as_mut() else {
        return;
    };
    core.control(
        embassy_time::Instant::now().as_micros(),
        FROM_BUTTON,
        &buf[..n],
        &mut reply,
        &mut imm,
    );
    debug_assert!(imm.is_none(), "IDENTIFY does not write to flash");
}
