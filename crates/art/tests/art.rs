//! The art crate's integration tests, in one test binary.
//!
//! `cargo test` runs test binaries one after another and the tests inside one
//! binary side by side; as a binary each, these four ran in a row.
//!
//! One file's tests: `cargo test -p screeny-art --test art pinned_time::`.

mod dark_ramp;
mod pinned_time;
mod rate;
mod sender;
