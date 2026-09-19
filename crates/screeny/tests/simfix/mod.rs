//! Shared fixture: a `screeny-sim` device on loopback, and a `Device` that
//! points at it.
//!
//! Every test in this crate that needs a receiver uses `screeny-sim` - the
//! second, independent implementation of the protocol (card 006) - rather than
//! a fake built here, because an exactness claim is only worth something if
//! the thing checking it did not learn the wire format from this crate.
//!
//! Ephemeral ports, loopback, **mDNS off**. No test in this file or any file
//! that includes it may touch the bench device.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;

use screeny::{Device, Sender, SenderConfig};
use screeny_sim::{Config, SimDevice, SimHandle};

/// Nothing here waits longer than this. A test that hangs is a test that
/// wastes an afternoon.
pub const PATIENCE: Duration = Duration::from_secs(2);

/// A simulator configured for tests, optionally on chosen ports so a restart
/// can land on the same ones.
#[must_use]
pub fn sim_config(frame_port: u16, control_port: u16) -> Config {
    Config {
        frame_port,
        control_port,
        // Compressed so a test never waits out the spec's ten-second HOLD.
        timing: screeny_sim::Timing {
            hold_ms: 300,
            fade_ms: 50,
            ..screeny_sim::Timing::SPEC
        },
        ..Config::for_test()
    }
}

/// A `Device` pointing at a running simulator.
///
/// Built by hand rather than with `Device::from_addr`, which guesses the
/// control port as frame + 1: true of the spec's defaults and never true of
/// the ephemeral pair a test gets.
#[must_use]
pub fn device_for(frame: SocketAddr, control: SocketAddr) -> Device {
    Device {
        instance: "screeny-sim".into(),
        host: None,
        frame,
        control,
        addresses: vec![frame.ip()],
        info: None,
    }
}

/// Start a simulator on ephemeral ports and connect a sender to it.
pub fn connect(cfg: SenderConfig) -> (SimDevice, SimHandle, Sender) {
    let dev = SimDevice::start(sim_config(0, 0)).expect("sim starts");
    let sim = dev.handle();
    let target = device_for(dev.frame_addr(), dev.control_addr());
    let sender = Sender::connect(target, cfg).expect("sender connects");
    (dev, sim, sender)
}

/// Expand a palette and indices the way the panel must: `palette[index]`, and
/// nothing else. The tests compare the simulator's decoded frame against this.
#[must_use]
pub fn expand(palette: &[[u8; 3]], indices: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(indices.len() * 3);
    for &i in indices {
        out.extend_from_slice(&palette[i as usize]);
    }
    out
}
