//! Every integration test of the simulator, in one test binary.
//!
//! `cargo test` runs test binaries one after another and the tests inside one
//! binary side by side. These files were a binary each, and most of what they
//! do is wait on real time - rate limits, LOCK_MS, the stream timeout - so in
//! a row they were half a minute of every `cargo test`. As modules of one
//! binary they wait together; each was already written for a loaded machine
//! (ephemeral loopback ports, a deadline on every wait).
//!
//! One file's tests: `cargo test -p screeny-sim --test sim faults::`.

mod common;

mod arbitration;
mod button;
mod cli;
mod codecs;
mod conformance;
mod control;
mod core_rules;
mod faults;
mod health;
mod http_conformance;
mod http_routes;
mod http_wifi;
mod malformed;
mod ota_watch;
mod sequence;
mod stall_234;
mod telemetry;
