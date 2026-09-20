//! `screeny-probe` as a library: the sender, the encoders, the vectors and
//! the wire-level conformance suite.
//!
//! The binary of the same name is the CLI over all of this. It is a library
//! as well as a binary because `crates/sim`'s integration tests need the same
//! sender and the same section-4 encoders, and card 080's rule was that there
//! be one copy of each rather than two that drift.
//!
//! Everything here is built on `screeny-proto` and nothing else, so a
//! disagreement between the probe and a device can only mean one of them
//! disagrees with `docs/design/protocol-v1.md`.

pub mod enc;
pub mod link;
pub mod suite;
pub mod vectors;

use screeny_proto::control::state as tstate;

/// The name of a telemetry `state` byte (section 6.7).
pub fn state_name(b: u8) -> &'static str {
    match b {
        tstate::IDLE => "IDLE",
        tstate::LIVE => "LIVE",
        tstate::HOLD => "HOLD",
        tstate::IDENTIFY => "IDENTIFY",
        tstate::PROVISIONING => "PROVISIONING",
        _ => "?",
    }
}
