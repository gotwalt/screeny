//! Every integration test of the studio, in one test binary.
//!
//! `cargo test` runs test binaries one after another and the tests inside one
//! binary side by side. These files were a binary each, and most of what they
//! do is wait - out a fade, a supervisor tick, a status poll - so run in a row
//! they were a minute and a half of every `cargo test` spent mostly asleep.
//! As modules of one binary they wait together. Each file is still its own
//! module with its own helpers, and is written for a loaded machine (every
//! server is on an ephemeral loopback port, every wait has a deadline).
//!
//! One file's tests: `cargo test -p screeny-studio --test studio panel::`.

mod common;

mod api;
mod app;
mod channels;
mod device_health;
mod device_status;
mod firmware;
mod fleet;
mod ha_mqtt;
mod memory;
mod moved;
mod pacing;
mod panel;
mod preview;
mod soak;
mod ssid;
mod state_export;
mod traffic;
mod ui;
