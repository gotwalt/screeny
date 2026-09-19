//! Telemetry counters and the two EWMAs of spec section 6.8.
//!
//! One implementation, in [`screeny_receiver::stats`], shared with the
//! firmware (card 016). It is integer-only and wrapping - the counters are
//! `u32` and wrap, the EWMAs are `u16` and saturate - because the point of the
//! simulator is to be indistinguishable from the device, and a float would
//! quietly disagree with it in the last digit.
//!
//! This module is the name the simulator's callers already use.

pub use screeny_receiver::stats::{Arrival, Counters, Stats, Timings};
