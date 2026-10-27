//! The sender's integration tests, in one test binary - all but `pacing.rs`.
//!
//! `cargo test` runs test binaries one after another and the tests inside one
//! binary side by side; these were a binary each, mostly waiting on loopback
//! streams, so they wait together now. `pacing.rs` stays a binary of its own:
//! it measures the pacer against the wall clock, and nothing else should be
//! running in its process while it does.
//!
//! One file's tests: `cargo test -p screeny --test host loopback::`.

mod common;
mod simfix;

mod cli;
mod color;
mod embed;
mod encode;
mod improv;
mod indexed;
mod loopback;
mod traffic;
