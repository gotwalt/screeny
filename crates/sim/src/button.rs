//! The button, on a host (card 230).
//!
//! The device has one button on GPIO15 and the simulator has no pins, so what
//! it has instead is this: the **same** recogniser the firmware runs
//! ([`screeny_provision::button`]), driven with synthetic timestamps. A test
//! can hold the button for five seconds in a microsecond, and what it proves
//! is what the firmware does, because there is one implementation of the
//! gesture rules and neither side has a second opinion.
//!
//! The clock is the recogniser's own and is advanced only by a press. It is
//! deliberately not the simulator's wall clock: a five-second hold that really
//! took five seconds would put five seconds into every test that exercises the
//! portal flow. The *screens* it raises do expire on the simulator's real
//! clock, because they are drawn into a window somebody may be looking at.

use screeny_provision::button::{ButtonEvent, Level, Recognizer, DEBOUNCE_MS};
use screeny_provision::Screen;

/// How often the firmware's task polls the pin while a gesture is in flight.
/// The same cadence here, so the two see the same instants.
const POLL_MS: u32 = 50;

/// How long "cancelled" and the refusal stay up, as in `firmware/src/button.rs`.
const CANCELLED_MS: u64 = 1_200;
const REFUSED_MS: u64 = 2_500;

/// What the panel is showing on account of the button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Countdown(u8),
    /// The transient screens, with the simulator-clock instant they end at.
    Cancelled(u64),
    Refused(u64),
}

/// The button and what it has put on the panel.
#[derive(Debug, Clone)]
pub struct ButtonModel {
    rec: Recognizer,
    /// The recogniser's private millisecond clock. See the module docs.
    clock_ms: u32,
    shown: Option<Shown>,
}

impl Default for ButtonModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ButtonModel {
    /// A button at rest.
    #[must_use]
    pub fn new() -> Self {
        let mut rec = Recognizer::new();
        // The first reading of a pin at rest, which is what an ordinary boot
        // gives the firmware's task.
        let _ = rec.poll(Level::High, 0, true);
        ButtonModel {
            rec,
            clock_ms: 0,
            shown: None,
        }
    }

    /// Press the button, hold it for `hold_ms`, and let it go.
    ///
    /// Returns everything the recogniser said, in order, so the caller can
    /// carry the gestures out. `now_us` is the simulator's clock and is used
    /// only for the screens' own deadlines.
    pub fn press(&mut self, hold_ms: u32, now_us: u64, wipe_allowed: bool) -> Vec<ButtonEvent> {
        let mut out = Vec::new();
        self.feed(Level::Low, 0, wipe_allowed, &mut out);
        let mut held = 0;
        while held + POLL_MS <= hold_ms {
            held += POLL_MS;
            self.feed(Level::Low, POLL_MS, wipe_allowed, &mut out);
        }
        // The release lands at exactly `hold_ms`, whatever the poll cadence,
        // and is believed one debounce window later.
        self.feed(Level::High, hold_ms - held, wipe_allowed, &mut out);
        self.feed(Level::High, DEBOUNCE_MS, wipe_allowed, &mut out);
        for ev in &out {
            self.note(*ev, now_us);
        }
        out
    }

    fn feed(&mut self, level: Level, advance_ms: u32, wipe_allowed: bool, out: &mut Vec<ButtonEvent>) {
        self.clock_ms = self.clock_ms.wrapping_add(advance_ms);
        let ev = self.rec.poll(level, self.clock_ms, wipe_allowed);
        if ev != ButtonEvent::None {
            out.push(ev);
        }
    }

    /// Keep the panel in step with the gesture.
    fn note(&mut self, ev: ButtonEvent, now_us: u64) {
        self.shown = match ev {
            ButtonEvent::HoldStarted { seconds_left } | ButtonEvent::HoldTick { seconds_left } => {
                Some(Shown::Countdown(seconds_left))
            }
            ButtonEvent::HoldCancelled => Some(Shown::Cancelled(now_us + CANCELLED_MS * 1_000)),
            ButtonEvent::HoldRefused => Some(Shown::Refused(now_us + REFUSED_MS * 1_000)),
            ButtonEvent::WipeWifi => None,
            _ => return,
        };
    }

    /// What the button wants on the panel, if anything.
    ///
    /// The same rank the firmware gives it in `net.rs`: above the portal
    /// screen, below an update.
    #[must_use]
    pub fn screen(&self, now_us: u64) -> Option<Screen<'static>> {
        match self.shown? {
            Shown::Countdown(seconds_left) => Some(Screen::WipeCountdown { seconds_left }),
            Shown::Cancelled(until) if now_us < until => Some(Screen::WipeCancelled),
            Shown::Refused(until) if now_us < until => Some(Screen::WipeUnavailable),
            _ => None,
        }
    }
}
