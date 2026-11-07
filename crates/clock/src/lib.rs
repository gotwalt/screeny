//! The twenty-four clocks, with no opinion about where they run.
//!
//! `crates/art` shows them through the Studio; the firmware runs them on the
//! panel by itself. Both turn the same hands with the same choreography, which
//! is why this is one crate and not a port: a new dance reaches both.
//!
//! `no_std` and `alloc`: a composition is a few `Vec`s and `String`s, made once
//! a minute. Nothing here reads a clock or touches a socket - the time and the
//! bytes are handed in - so all of it is tested by a plain `cargo test`.

#![cfg_attr(not(any(feature = "std", test)), no_std)]

extern crate alloc;

pub mod ambient;
pub mod dance;
pub mod engine;
pub mod face;
mod math;
pub mod raster;
pub mod rng;
pub mod sntp;
pub mod tz;
pub mod variety;
