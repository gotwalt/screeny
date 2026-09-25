//! The button gesture recogniser: levels and milliseconds in, gestures out.
//!
//! The Tidbyt Gen 1 has one button, on GPIO15, active low
//! (`docs/research/008-button.md`, confirmed on the bench with the button
//! pressed, card 203). Card 230 gives it two jobs, and device-web decision 10
//! narrows them to exactly these two:
//!
//! | held for | what happens |
//! |---|---|
//! | < [`DEBOUNCE_MS`] | nothing: it is a bounce |
//! | up to [`HOLD_MS`], then released | [`ButtonEvent::ShortPress`] - the status screen for [`STATUS_MS`] |
//! | past [`HOLD_MS`] | [`ButtonEvent::HoldStarted`], then a [`ButtonEvent::HoldTick`] a second |
//! | released during the countdown | [`ButtonEvent::HoldCancelled`] - nothing is changed |
//! | [`WIPE_MS`] | [`ButtonEvent::WipeWifi`] - forget the network, raise the portal |
//!
//! There is no 15 s factory reset and no held-at-boot ladder: the author
//! dropped both (device-web decision 10), and a pin that is already low when
//! the firmware starts is **ignored until it is released** - see
//! [`Recognizer::poll`].
//!
//! # Why it is here
//!
//! Same reason as [`crate::machine`]: this is the half of the button that can
//! be right before it meets hardware. The firmware's task owns an `Input` and
//! a clock and nothing else; the simulator drives the same recogniser with
//! synthetic timestamps. `no_std`, no allocation, no clock of its own, and
//! every comparison wrapping, because `now_ms` is a `u32` that wraps every
//! 49.7 days and this device is meant to run for months.
//!
//! # Example
//!
//! ```
//! use screeny_provision::button::{ButtonEvent, Level, Recognizer, HOLD_MS, WIPE_MS};
//!
//! let mut b = Recognizer::new();
//! // The pin rests high. The first poll settles the recogniser.
//! assert_eq!(b.poll(Level::High, 0, true), ButtonEvent::None);
//! // Pressed, then released 200 ms later: a short press.
//! assert_eq!(b.poll(Level::Low, 1_000, true), ButtonEvent::None);
//! assert_eq!(b.poll(Level::Low, 1_040, true), ButtonEvent::None);
//! assert_eq!(b.poll(Level::High, 1_200, true), ButtonEvent::None); // debouncing
//! assert_eq!(b.poll(Level::High, 1_240, true), ButtonEvent::ShortPress);
//! ```

/// A bounce shorter than this is not a level change (008: one 11 ms glitch in
/// eleven presses on this unit; the stock firmware waits 100 ms and that is
/// long enough to make a short press feel slow).
pub const DEBOUNCE_MS: u32 = 30;

/// Held this long and it is no longer a press: the countdown starts.
pub const HOLD_MS: u32 = 1_000;

/// Held this long and the stored credentials go.
pub const WIPE_MS: u32 = 5_000;

/// How long a short press leaves the status screen up.
///
/// Here rather than in the firmware because the simulator shows the same
/// screen for the same time, and because a caller that reuses the `IDENTIFY`
/// overlay (the firmware does) has to hand it a duration in milliseconds.
pub const STATUS_MS: u32 = 10_000;

const _: () = assert!(HOLD_MS < WIPE_MS);
const _: () = assert!(DEBOUNCE_MS < HOLD_MS);

/// The pin as read, not as interpreted: **the button is active low**, so
/// [`Level::Low`] is pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// At rest - nobody is touching it.
    High,
    /// Pressed.
    Low,
}

impl Level {
    /// The reading as the firmware's `Input::is_low()` produces it.
    #[must_use]
    pub const fn from_is_low(is_low: bool) -> Self {
        if is_low {
            Level::Low
        } else {
            Level::High
        }
    }

    const fn pressed(self) -> bool {
        matches!(self, Level::Low)
    }
}

/// What one [`Recognizer::poll`] decided.
///
/// At most one per poll: the caller polls often enough (the firmware on every
/// edge and every 100 ms while a gesture is in flight) that there is never a
/// second one to lose, and a queue in a `no_std` recogniser would be a buffer
/// to size for no reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ButtonEvent {
    /// Nothing happened.
    None,
    /// Pressed and released inside [`HOLD_MS`]: show the status screen.
    ShortPress,
    /// Held past [`HOLD_MS`]. The countdown is now on the panel and the
    /// number on it is `seconds_left`.
    HoldStarted {
        /// Whole seconds until [`WIPE_MS`], rounded up: 4 at the start.
        seconds_left: u8,
    },
    /// The countdown's number changed.
    HoldTick {
        /// Whole seconds until [`WIPE_MS`], rounded up.
        seconds_left: u8,
    },
    /// Let go before [`WIPE_MS`]: nothing is changed.
    HoldCancelled,
    /// A hold reached the point where the countdown would start (or where the
    /// wipe would happen) while `wipe_allowed` was false.
    ///
    /// Card 230: a firmware update is in flight or on trial, and forgetting
    /// the network then would orphan the upload or sabotage the health check
    /// that is the only thing standing between this device and a rollback.
    /// The gesture is refused, the caller says why on the panel, and nothing
    /// further happens until the button is released.
    HoldRefused,
    /// Held for [`WIPE_MS`]: forget the credentials and raise the portal.
    WipeWifi,
}

/// Where a gesture has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Nothing has been released yet, so nothing counts. This is where a
    /// recogniser starts, which is what makes a pin that is already low when
    /// the firmware boots (a stuck button, or somebody holding it while the
    /// cable goes in) harmless: the ladder cannot start until the button has
    /// been seen to be up.
    WaitRelease,
    /// At rest.
    Up,
    /// Pressed, and still inside [`HOLD_MS`].
    Down {
        /// When the press settled.
        since: u32,
    },
    /// Past [`HOLD_MS`]: the countdown is on the panel.
    Counting {
        /// When the press settled - the same instant [`Phase::Down`] held.
        since: u32,
        /// The number last reported, so a tick is only sent when it changes.
        shown: u8,
    },
    /// The hold was refused (see [`ButtonEvent::HoldRefused`]); waiting for
    /// the button to come up.
    Refused,
    /// The wipe has fired; waiting for the button to come up. One press, one
    /// wipe: leaning on the button does not do it twice.
    Fired,
}

/// The gesture recogniser. One per device.
#[derive(Debug, Clone)]
pub struct Recognizer {
    phase: Phase,
    /// The level as last read, before debouncing.
    raw: Level,
    /// When `raw` last changed.
    raw_since: u32,
    /// The level the recogniser believes in.
    stable: Level,
}

impl Default for Recognizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrapping elapsed milliseconds, `then` to `now`.
fn since(now: u32, then: u32) -> u32 {
    now.wrapping_sub(then)
}

/// Whole seconds left of the hold, rounded up, clamped to 1.
fn seconds_left(elapsed: u32) -> u8 {
    let left = WIPE_MS.saturating_sub(elapsed);
    let secs = left.div_ceil(1_000).max(1);
    // `WIPE_MS - HOLD_MS` is 4 s, so this is 1..=4 and fits a digit.
    secs.min(9) as u8
}

impl Recognizer {
    /// A recogniser that has not seen the pin yet.
    #[must_use]
    pub const fn new() -> Self {
        Recognizer {
            phase: Phase::WaitRelease,
            raw: Level::High,
            raw_since: 0,
            stable: Level::High,
        }
    }

    /// Is a gesture (or a bounce) in flight?
    ///
    /// The firmware task sleeps on the pin's interrupt when this is false and
    /// polls on a short timer when it is true, so an untouched button costs
    /// the frame path nothing at all (device-web decision 7).
    #[must_use]
    pub fn active(&self) -> bool {
        self.raw != self.stable || !matches!(self.phase, Phase::Up)
    }

    /// Feed one reading.
    ///
    /// `now_ms` is any monotonic millisecond clock and may wrap;
    /// `wipe_allowed` is the caller's answer to "would forgetting the network
    /// right now break something?" - the firmware says no while a firmware
    /// upload is in flight or an image is on trial. It is read at the moment
    /// the countdown would start and again at the moment the wipe would fire,
    /// so an upload that begins mid-hold still stops the wipe.
    pub fn poll(&mut self, level: Level, now_ms: u32, wipe_allowed: bool) -> ButtonEvent {
        // --- debounce -------------------------------------------------
        if level != self.raw {
            self.raw = level;
            self.raw_since = now_ms;
        }
        let mut edge = None;
        if self.raw != self.stable && since(now_ms, self.raw_since) >= DEBOUNCE_MS {
            self.stable = self.raw;
            edge = Some(self.stable);
        }

        // A pin that has been at rest since the recogniser was built is where
        // an ordinary boot starts, and the first poll is what says so. Both
        // readings have to agree: a recogniser built while the button is held
        // sees `stable` high and `raw` low on its very first poll, and leaving
        // here on that would arm the ladder for a press nobody made.
        if self.phase == Phase::WaitRelease
            && self.stable == Level::High
            && self.raw == Level::High
        {
            self.phase = Phase::Up;
            return ButtonEvent::None;
        }

        // --- edges ----------------------------------------------------
        // Edges are timed from [`Recognizer::raw_since`] - the instant the pin
        // actually moved - and not from the poll that noticed. Otherwise every
        // duration in the ladder carries the debounce window plus however late
        // the poll was, and "999 ms or 1,000 ms?" would be answered by the
        // scheduler.
        match edge {
            Some(Level::Low) => {
                if let Phase::Up = self.phase {
                    self.phase = Phase::Down {
                        since: self.raw_since,
                    };
                }
                return ButtonEvent::None;
            }
            Some(Level::High) => {
                let was = self.phase;
                let released = self.raw_since;
                self.phase = Phase::Up;
                return match was {
                    // Released before the countdown could start. The duration
                    // is measured rather than assumed: a caller that stopped
                    // polling while the button was down (nothing in this
                    // repository does) gets `HoldCancelled` for a long press
                    // rather than a surprise `ShortPress`, and never a wipe it
                    // did not watch count down.
                    Phase::Down { since: at } => {
                        if since(released, at) < HOLD_MS {
                            ButtonEvent::ShortPress
                        } else {
                            ButtonEvent::HoldCancelled
                        }
                    }
                    Phase::Counting { .. } => ButtonEvent::HoldCancelled,
                    _ => ButtonEvent::None,
                };
            }
            None => {}
        }

        // --- timers, while the button is genuinely down ----------------
        if !self.stable.pressed() {
            return ButtonEvent::None;
        }
        match self.phase {
            Phase::Down { since: at } => {
                let held = since(now_ms, at);
                if held < HOLD_MS {
                    return ButtonEvent::None;
                }
                if !wipe_allowed {
                    self.phase = Phase::Refused;
                    return ButtonEvent::HoldRefused;
                }
                // A poll that arrives late (nothing ran for a second) can
                // cross both thresholds at once. Start the countdown anyway
                // and let the next poll fire the wipe: the panel must always
                // have said what was about to happen.
                let left = seconds_left(held.min(WIPE_MS - 1));
                self.phase = Phase::Counting {
                    since: at,
                    shown: left,
                };
                ButtonEvent::HoldStarted { seconds_left: left }
            }
            Phase::Counting { since: at, shown } => {
                let held = since(now_ms, at);
                if held >= WIPE_MS {
                    if !wipe_allowed {
                        self.phase = Phase::Refused;
                        return ButtonEvent::HoldRefused;
                    }
                    self.phase = Phase::Fired;
                    return ButtonEvent::WipeWifi;
                }
                let left = seconds_left(held);
                if left != shown {
                    self.phase = Phase::Counting {
                        since: at,
                        shown: left,
                    };
                    return ButtonEvent::HoldTick { seconds_left: left };
                }
                ButtonEvent::None
            }
            _ => ButtonEvent::None,
        }
    }
}
