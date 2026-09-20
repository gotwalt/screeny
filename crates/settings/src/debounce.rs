//! When a changed setting should reach flash.
//!
//! Pure logic, no clock and no I/O, exactly like `screeny-receiver`: the caller
//! passes `now_ms`. The task that sleeps, wakes and calls [`Store`] is the
//! firmware's (card 212).
//!
//! [`Store`]: crate::Store

/// How long a setting must sit unchanged before it is written.
///
/// Three seconds is card 063's number. A `SET_BRIGHTNESS` slider sends about
/// sixty changes a second; at 3 s of quiet a drag of any length costs exactly
/// one write, taken when the finger comes off.
pub const QUIET_MS: u64 = 3_000;

/// The settings this policy applies to.
///
/// **There is deliberately no Wi-Fi variant.** Credentials are written
/// immediately by [`Store::save_wifi`], before the device drops its current
/// association, so they cannot be debounced by construction rather than by a
/// comment.
///
/// [`Store::save_wifi`]: crate::Store::save_wifi
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Field {
    /// The friendly name.
    Name = 0,
    /// The panel brightness.
    Brightness = 1,
    /// The idle mode.
    IdleMode = 2,
}

impl Field {
    /// Every field, in order. Handy for a caller that wants to drain.
    pub const ALL: [Field; 3] = [Field::Name, Field::Brightness, Field::IdleMode];

    const fn index(self) -> usize {
        self as usize
    }
}

/// One pending-write timer per field.
///
/// The firmware keeps the live value wherever it already lives (the
/// `BRIGHTNESS` atomic, the receiver's `Core`) and uses this only to decide
/// *when* to copy it to flash. It holds no values, so it cannot disagree with
/// the live state.
///
/// Repeated changes to the same field coalesce: each one restarts that field's
/// timer, so a setting that is still moving is never written. Different fields
/// are independent.
#[derive(Debug, Clone, Copy, Default)]
pub struct Debounce {
    /// `Some(t)` = this field changed at `t` and has not been committed.
    changed_at: [Option<u64>; 3],
}

impl Debounce {
    /// Nothing pending.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            changed_at: [None; 3],
        }
    }

    /// Record that `field` changed at `now_ms`. Restarts that field's timer.
    pub fn note_change(&mut self, field: Field, now_ms: u64) {
        self.changed_at[field.index()] = Some(now_ms);
    }

    /// Forget a pending change without writing it, e.g. because the value went
    /// back to what flash already holds.
    pub fn cancel(&mut self, field: Field) {
        self.changed_at[field.index()] = None;
    }

    /// The next field that has been quiet for [`QUIET_MS`], if any, clearing
    /// it. Call it in a loop until it answers `None`.
    pub fn due(&mut self, now_ms: u64) -> Option<Field> {
        for field in Field::ALL {
            if let Some(at) = self.changed_at[field.index()] {
                if now_ms.saturating_sub(at) >= QUIET_MS {
                    self.changed_at[field.index()] = None;
                    return Some(field);
                }
            }
        }
        None
    }

    /// True when nothing is waiting to be written.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.changed_at.iter().all(Option::is_none)
    }

    /// How long to sleep before calling [`Debounce::due`] again: `Some(0)`
    /// when something is already due, `None` when there is nothing pending and
    /// the task can wait for a change instead of a timer.
    #[must_use]
    pub fn next_due_in_ms(&self, now_ms: u64) -> Option<u64> {
        self.changed_at
            .iter()
            .flatten()
            .map(|at| QUIET_MS.saturating_sub(now_ms.saturating_sub(*at)))
            .min()
    }
}
