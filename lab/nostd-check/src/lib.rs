//! Proof that the decode paths are firmware-ready.
//!
//! This crate is `#![no_std]`, declares no allocator, and pulls in the lab's
//! decoder module tree by path. If it compiles, every decoder in
//! `lab/src/dec/` is free of `std`, free of `alloc`, and free of floating
//! point beyond what `core` provides (we grep for that separately -- see
//! `lab/nostd-check/README.md`).
#![no_std]
#![deny(unsafe_code)]

#[path = "../../src/dec/mod.rs"]
pub mod dec;

/// Force every decoder to be code-generated so unused-code elimination cannot
/// hide a `std` dependency.
pub fn touch(src: &[u8], dst: &mut [u8; dec::NBYTES]) -> Result<(), dec::DecErr> {
    dec::decode(src, dst)
}
