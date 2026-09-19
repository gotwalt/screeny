//! Counts the Xtensa instructions a frame decode actually executes.
//!
//! Built for `xtensa-esp32-none-elf` and run under `qemu-system-xtensa
//! -machine sim`. The binary decodes one payload `BENCH_REPS` times and then
//! exits via the simulator `simcall`; `run.sh` builds it twice with different
//! rep counts and takes the difference, so startup, the payload load and the
//! exit all cancel and what is left is the cost of one decode.
//!
//! qemu's `sim` machine models a dc232b, not the ESP32's LX6. That is fine for
//! *counting* instructions -- the code here is base-ISA integer work, no
//! ESP32-only opcodes -- but it says nothing about cycles. See the report for
//! how the instruction count is turned into a time estimate.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

#[path = "../../src/dec/mod.rs"]
mod dec;
mod vectors;

use core::arch::{asm, global_asm};

// Startup, in two pieces.
//
// `.text.reset` is placed at the dc232b reset vector (0xfe000000) by
// `link.x`, because qemu starts the core there and ignores the ELF entry
// point. It jumps to `_start`, which lives at 0xd0002000 -- the only window a
// dc232b has mapped coming out of reset is 0xd0000000 onto physical 0.
//
// `_start` also has to do what a real runtime's reset handler does before any
// Rust code can run: a core comes out of reset with PS.WOE clear and PS.EXCM
// set, so the windowed-ABI `entry` instruction that opens every compiled
// function faults. Setting WindowBase/WindowStart and PS.WOE fixes that.
//
// Every immediate below is <= 2047, so the assembler emits `movi` rather than
// `l32r`. That matters: neither of these sections has a literal pool, and an
// l32r can only reach literals at lower addresses anyway.
global_asm!(
    r#"
    .section .text.reset,"ax",@progbits
    .global _reset
    .align 4
_reset:
    movi    a0, 0xd0
    slli    a0, a0, 24
    addmi   a0, a0, 0x2000
    jx      a0

    .section .text.start,"ax",@progbits
    .global _start
    .align 4
_start:
    movi    a0, 0
    wsr.windowbase a0
    rsync
    movi    a0, 1
    wsr.windowstart a0
    rsync
    movi    a0, 4
    slli    a0, a0, 16
    wsr.ps  a0
    rsync
    movi    a1, 0xd0
    slli    a1, a1, 24
    movi    a2, 0x200
    slli    a2, a2, 12
    add     a1, a1, a2
    movi    a0, 0
    call4   rust_main
1:  j       1b
"#
);

static PAYLOAD: &[u8] = include_bytes!(env!("BENCH_PAYLOAD"));
const REPS: u32 = match u32::from_str_radix(env!("BENCH_REPS"), 10) {
    Ok(v) => v,
    Err(_) => 1,
};

static mut FRAME: [u8; dec::NBYTES] = [0u8; dec::NBYTES];

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    let mut acc = 0u32;
    for _ in 0..REPS {
        let dst = unsafe { &mut *core::ptr::addr_of_mut!(FRAME) };
        // `black_box` the payload so nothing is hoisted or constant-folded.
        let src: &[u8] = unsafe { core::ptr::read_volatile(&PAYLOAD) };
        if dec::decode(src, dst).is_ok() {
            acc = acc.wrapping_add(dst[0] as u32).wrapping_add(dst[6143] as u32);
        }
    }
    exit(acc & 1)
}

/// Xtensa simulator call: a2 = syscall number (1 = exit), a3 = argument.
fn exit(code: u32) -> ! {
    unsafe {
        asm!(
            "movi a2, 1",
            "mov  a3, {c}",
            "simcall",
            c = in(reg) code,
            options(noreturn)
        )
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit(2)
}
