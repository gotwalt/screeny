//! Telemetry counters and the two EWMAs of spec section 6.8.
//!
//! A `no_std` copy of `crates/sim/src/stats.rs` (card 006), which was written
//! integer-only and wrapping *specifically* so that the device and the
//! simulator would agree in the last digit. Copying rather than sharing is
//! what card 008 was told to do: lifting it into `crates/proto` would mean
//! changing `crates/sim` in the same breath, and card 009 is in flight there.
//! The two files should be diffed if either changes.

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Arrival {
    prev_us: Option<u64>,
    /// EWMA of per-frame inter-arrival time.
    pub interarrival_us: u16,
    /// EWMA of `|d_i - interarrival_us|`.
    pub jitter_us: u16,
    /// Max `d_i` since `RESET_STATS`.
    pub interarrival_max_us: u16,
    seeded: bool,
}

impl Arrival {
    /// Forget the previous arrival and both EWMAs, as a source change must.
    ///
    /// `interarrival_max_us` is deliberately *not* cleared: section 6.8 makes
    /// it "max since reset", and the reset it means is `RESET_STATS`.
    pub fn on_source_change(&mut self) {
        self.prev_us = None;
        self.interarrival_us = 0;
        self.jitter_us = 0;
        self.seeded = false;
    }

    /// Record the arrival of an accepted frame. `n` is `seq_i - seq_{i-1}`,
    /// so a lost frame widens the interval rather than counting as jitter.
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

/// The same EWMA over decode time, plus the two maxima.
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
    /// Zero every counter, as `RESET_STATS` does. The stream state, the source
    /// lock and `last_seq` are deliberately left alone: it is a measurement
    /// control, not a stream control (section 6.8).
    pub fn reset(&mut self) {
        let prev = self.arrival.prev_us;
        *self = Stats::default();
        self.arrival.prev_us = prev;
    }

    /// Build the 48-byte struct of section 6.7.
    #[allow(clippy::too_many_arguments)]
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
