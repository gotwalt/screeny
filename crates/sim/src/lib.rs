//! A fake screeny panel.
//!
//! `screeny-sim` is a second, independent implementation of the receiver half
//! of [`docs/design/protocol-v1.md`](../../../docs/design/protocol-v1.md): it
//! binds the two UDP ports, runs the source-lock and idle state machine of
//! section 7, decodes every v1 codec through
//! [`screeny_proto`](screeny_proto), answers every control op of section 6,
//! and advertises itself over mDNS. There is one Tidbyt on this bench and it
//! has one owner; everybody else develops against this.
//!
//! Nothing in this crate implements any wire format. Every byte that goes out
//! and every byte that comes in passes through `screeny-proto`, so the
//! simulator and the firmware cannot drift apart in the parser - only in the
//! *behaviour* around it, which is exactly the disagreement a second
//! implementation exists to find.
//!
//! # Library
//!
//! ```no_run
//! use std::time::Duration;
//! use screeny_sim::{Config, SimDevice};
//!
//! // Loopback, ephemeral ports, no mDNS: what a test wants.
//! let dev = SimDevice::start(Config::for_test())?;
//! let sim = dev.handle();
//!
//! // ... send FRAME datagrams to sim.frame_addr() ...
//!
//! if let Some(shot) = sim.wait_for_frames(1, Duration::from_secs(1)) {
//!     assert_eq!(shot.telemetry.frames_shown, 1);
//!     let _pixels: &[u8] = &shot.decoded[..]; // bit-exact, no panel model
//! }
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! [`SimHandle`] is the whole API: [`SimHandle::snapshot`] for the displayed
//! frame and the counters, [`SimHandle::wait_for_frames`] and
//! [`SimHandle::wait_until`] to wait on them, [`SimHandle::events`] and
//! [`SimHandle::wait_for`] for the things counters do not record, and
//! [`SimHandle::set_faults`] to make the device misbehave on purpose.
//!
//! # Binary
//!
//! ```text
//! screeny-sim                      # LED-dot window on the default ports
//! screeny-sim --headless --mdns    # no window, stats once a second
//! screeny-sim --help
//! ```
//!
//! # What it is not
//!
//! It is not a renderer of what the panel *looks like* to a camera. The
//! window applies the panel model of the generative-art brief - sRGB, linear,
//! 64 duty levels, back to sRGB - and draws round dots on black, which is
//! close enough to judge dithering and thin lines by. It does not model
//! ghosting, refresh banding or the panel's actual primaries.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod core;
pub mod device;
pub mod dump;
pub mod event;
pub mod font;
pub mod mdns;
pub mod net;
pub mod panel;
pub mod screens;
pub mod stats;
#[cfg(feature = "window")]
pub mod window;

pub use config::{Config, Faults, PanelModel, Timing};
pub use core::{Core, FrameMeta, Outbox};
pub use device::{FrameSink, SimDevice, SimHandle, Snapshot};
pub use event::{DropCause, Event, ReleaseReason, State};
pub use stats::Stats;

/// The mDNS instance name the simulator advertises under.
///
/// Spec section 5.1 gives the *device* `screeny-<xxxxxx>`, and the Tidbyt on
/// this bench answers to `screeny`. The simulator must never be mistaken for
/// it, so it takes a name of its own and [`instance_is_reserved`] refuses the
/// collision rather than trusting anyone to remember.
pub const DEFAULT_INSTANCE: &str = "screeny-sim";

/// True if `name` is an instance name the simulator must not claim.
///
/// That is `screeny` itself, and `screeny-<six hex digits>`, which section
/// 5.1 reserves for real devices naming themselves after their MAC.
#[must_use]
pub fn instance_is_reserved(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    if lower == "screeny" || lower == "screeny.local" {
        return true;
    }
    match lower.strip_prefix("screeny-") {
        Some(rest) => {
            let rest = rest.strip_suffix(".local").unwrap_or(rest);
            rest.len() == 6 && rest.chars().all(|c| c.is_ascii_hexdigit())
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_devices_names_are_refused() {
        assert!(instance_is_reserved("screeny"));
        assert!(instance_is_reserved("SCREENY"));
        assert!(instance_is_reserved("screeny.local"));
        assert!(instance_is_reserved("screeny-a4cf12"));
        assert!(instance_is_reserved("screeny-A4CF12.local"));
    }

    #[test]
    fn the_simulators_own_names_are_allowed() {
        assert!(!instance_is_reserved(DEFAULT_INSTANCE));
        assert!(!instance_is_reserved("screeny-sim-2"));
        assert!(!instance_is_reserved("desk-sim"));
        // Six characters, but not hex: not a MAC suffix, so not reserved.
        assert!(!instance_is_reserved("screeny-zzzzzz"));
    }
}
