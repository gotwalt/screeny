//! Telemetry counters and the two EWMAs of spec section 6.8.
//!
//! Everything here is integer-only and wrapping: the counters are `u32` and
//! wrap, and the EWMAs are `u16` and saturate. A host has no trouble with
//! `f64`, but the simulator exists to be indistinguishable from the device,
//! and a float would quietly disagree with it in the last digit. One copy of
//! this arithmetic is the only way that promise holds (card 016; cards 006 and
//! 008 each had their own).

use screeny_proto::control::Telemetry;

/// The free-running counters of spec section 6.7.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// `FRAME` datagrams accepted from the active source.
    pub frames_rx: u32,
    /// Frames actually pushed to the panel.
    pub frames_shown: u32,
    /// `seq` not newer than `last_seq`.
    pub frames_dropped_stale: u32,
    /// A newer frame arrived in the same drain before this one was displayed.
    pub frames_dropped_superseded: u32,
    /// Decode failed, or the codec is unsupported or reserved.
    pub frames_dropped_decode: u32,
    /// Bad magic/version/type/length, or not the active source.
    pub frames_rejected: u32,
    /// Total count of sequence numbers never seen.
    pub seq_gaps: u32,
}

impl Counters {
    /// Add one, wrapping, as the spec's `u32` counters do.
    pub fn bump(v: &mut u32) {
        *v = v.wrapping_add(1);
    }
}

/// The RFC 3550 section 6.4.1 smoothing of spec section 6.8, in `u16`.
///
/// `interarrival_us` and `jitter_us` are both EWMAs with a shift of 4. The
/// spec gives the update rule but not the seed; this seeds from the first
/// sample rather than easing up from zero, which is what RFC 3550 does and
/// what the amended section 6.8 now says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Arrival {
    /// Arrival time of the previous accepted frame, microseconds.
    prev_us: Option<u64>,
    /// EWMA of per-frame inter-arrival time.
    pub interarrival_us: u16,
    /// EWMA of `|d_i - interarrival_us|`.
    pub jitter_us: u16,
    /// Max `d_i` since reset.
    pub interarrival_max_us: u16,
    seeded: bool,
}

impl Arrival {
    /// Forget the previous arrival and both EWMAs. Spec section 6.8 requires
    /// this whenever the active source changes.
    ///
    /// `interarrival_max_us` is *not* cleared here: it is "max since reset",
    /// and only `RESET_STATS` resets it.
    pub fn on_source_change(&mut self) {
        self.prev_us = None;
        self.interarrival_us = 0;
        self.jitter_us = 0;
        self.seeded = false;
    }

    /// Clear everything, as `RESET_STATS` does.
    pub fn reset(&mut self) {
        *self = Arrival::default();
    }

    /// Record the arrival of an accepted frame.
    ///
    /// `n` is `seq_i - seq_{i-1}`, so a lost frame widens the interval instead
    /// of counting as jitter. It is at least 1 for any accepted frame.
    pub fn record(&mut self, arrival_us: u64, n: u32) {
        let prev = match self.prev_us.replace(arrival_us) {
            Some(p) => p,
            None => return, // first frame of a stream: no interval yet
        };
        let n = n.max(1) as u64;
        let d = (arrival_us.saturating_sub(prev) / n).min(u16::MAX as u64) as u16;
        self.interarrival_max_us = self.interarrival_max_us.max(d);
        if !self.seeded {
            // Section 6.8: seed from the first sample, RFC 3550 style, rather
            // than easing up from zero.
            self.interarrival_us = d;
            self.jitter_us = 0;
            self.seeded = true;
            return;
        }
        self.interarrival_us = ewma(self.interarrival_us, d);
        let err = d.abs_diff(self.interarrival_us);
        self.jitter_us = ewma(self.jitter_us, err);
    }
}

/// `v += (sample - v) / 16`, in `u16`, without going negative or overflowing.
fn ewma(v: u16, sample: u16) -> u16 {
    if sample >= v {
        v.saturating_add((sample - v) / 16)
    } else {
        v - (v - sample) / 16
    }
}

/// The same EWMA over decode and render times, plus their maxima.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timings {
    /// EWMA of decode time.
    pub decode_us: u16,
    /// Max decode time since reset.
    pub decode_us_max: u16,
    /// Max time to push a decoded frame to the panel buffer.
    pub render_us_max: u16,
    seeded: bool,
}

impl Timings {
    /// Record one decode.
    pub fn decode(&mut self, us: u32) {
        let us = us.min(u16::MAX as u32) as u16;
        self.decode_us_max = self.decode_us_max.max(us);
        self.decode_us = if self.seeded {
            ewma(self.decode_us, us)
        } else {
            self.seeded = true;
            us
        };
    }

    /// Record one push to the panel buffer.
    ///
    /// The firmware does its pushing on the other core and reports byte 42
    /// from an atomic instead (see `Host::adjust_telemetry`); this is the
    /// simulator's path.
    pub fn render(&mut self, us: u32) {
        let us = us.min(u16::MAX as u32) as u16;
        self.render_us_max = self.render_us_max.max(us);
    }
}

/// Everything the `TELEMETRY` body needs, assembled from the pieces above.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    /// Section 6.7's counters.
    pub counters: Counters,
    /// Section 6.8's inter-arrival EWMAs.
    pub arrival: Arrival,
    /// Decode and render timings.
    pub timings: Timings,
}

impl Stats {
    /// Zero every counter, as `RESET_STATS` does.
    ///
    /// The stream state, the source lock and `last_seq` are deliberately left
    /// alone: `RESET_STATS` is a measurement control, not a stream control.
    pub fn reset(&mut self) {
        let prev = self.arrival.prev_us;
        *self = Stats::default();
        // Keep the previous arrival timestamp so the next frame still produces
        // a sane interval rather than a 0.
        self.arrival.prev_us = prev;
    }

    /// Build the 48-byte struct of section 6.7.
    #[must_use]
    pub fn telemetry(
        &self,
        uptime_ms: u32,
        rssi_dbm: i8,
        brightness: u8,
        state: u8,
        last_codec: u8,
    ) -> Telemetry {
        Telemetry {
            uptime_ms,
            frames_rx: self.counters.frames_rx,
            frames_shown: self.counters.frames_shown,
            frames_dropped_stale: self.counters.frames_dropped_stale,
            frames_dropped_superseded: self.counters.frames_dropped_superseded,
            frames_dropped_decode: self.counters.frames_dropped_decode,
            frames_rejected: self.counters.frames_rejected,
            seq_gaps: self.counters.seq_gaps,
            interarrival_us: self.arrival.interarrival_us,
            jitter_us: self.arrival.jitter_us,
            interarrival_max_us: self.arrival.interarrival_max_us,
            decode_us: self.timings.decode_us,
            decode_us_max: self.timings.decode_us_max,
            render_us_max: self.timings.render_us_max,
            rssi_dbm,
            brightness,
            state,
            last_codec,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_converges_and_never_overflows() {
        let mut v = 0u16;
        for _ in 0..500 {
            v = ewma(v, 33_333);
        }
        assert!((33_000..=33_333).contains(&v), "converged to {v}");
        for _ in 0..500 {
            v = ewma(v, 0);
        }
        assert!(v < 100, "decayed to {v}");
        assert_eq!(ewma(u16::MAX, u16::MAX), u16::MAX);
    }

    #[test]
    fn arrival_seeds_from_the_first_interval() {
        let mut a = Arrival::default();
        a.record(0, 1);
        assert_eq!(a.interarrival_us, 0, "no interval from one sample");
        a.record(33_333, 1);
        assert_eq!(a.interarrival_us, 33_333, "seeded, not eased up from zero");
        assert_eq!(a.jitter_us, 0);
        assert_eq!(a.interarrival_max_us, 33_333);
    }

    #[test]
    fn a_lost_frame_widens_the_interval_instead_of_being_jitter() {
        let mut a = Arrival::default();
        a.record(0, 1);
        a.record(33_000, 1);
        // Two frame times later, but seq advanced by 2: d stays ~33 ms.
        a.record(99_000, 2);
        assert_eq!(a.interarrival_us, 33_000);
        assert_eq!(a.jitter_us, 0);
    }

    #[test]
    fn interarrival_saturates_rather_than_wrapping() {
        let mut a = Arrival::default();
        a.record(0, 1);
        a.record(10_000_000, 1);
        assert_eq!(a.interarrival_us, u16::MAX);
        assert_eq!(a.interarrival_max_us, u16::MAX);
    }

    #[test]
    fn source_change_clears_the_ewmas_but_not_the_max() {
        let mut a = Arrival::default();
        a.record(0, 1);
        a.record(33_333, 1);
        a.on_source_change();
        assert_eq!(a.interarrival_us, 0);
        assert_eq!(a.jitter_us, 0);
        assert_eq!(a.interarrival_max_us, 33_333);
        // And the next stream seeds afresh rather than measuring the gap
        // between two unrelated senders.
        a.record(5_000_000, 1);
        a.record(5_033_333, 1);
        assert_eq!(a.interarrival_us, 33_333);
    }
}
