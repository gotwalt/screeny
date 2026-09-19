//! The six Xtensa register-window spill/fill handlers.
//!
//! A real runtime (`xtensa-lx-rt`, ESP-IDF) installs these; this bench is not
//! using one, so a call nested more than a few frames deep -- `rust_main` ->
//! `decode` -> `dec_pal8_lz` -> `inflate` -- raises WindowOverflow4 at the
//! vector base and, with nothing there, double-faults into 0xd00003c0.
//!
//! These are the canonical handlers from the Xtensa ISA reference. `link.x`
//! pins them to the window vectors at 0xd0000000 + 0x00/0x40/0x80/0xc0/0x100/
//! 0x140. They are part of the *harness*, not of anything being measured: the
//! spills they perform are counted, which is honest, since the firmware will
//! pay the same cost.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .wvec.of4,"ax",@progbits
    .align 4
_WindowOverflow4:
    s32e    a0, a5, -16
    s32e    a1, a5, -12
    s32e    a2, a5, -8
    s32e    a3, a5, -4
    rfwo

    .section .wvec.uf4,"ax",@progbits
    .align 4
_WindowUnderflow4:
    l32e    a0, a5, -16
    l32e    a1, a5, -12
    l32e    a2, a5, -8
    l32e    a3, a5, -4
    rfwu

    .section .wvec.of8,"ax",@progbits
    .align 4
_WindowOverflow8:
    s32e    a0, a9, -16
    l32e    a0, a1, -12
    s32e    a1, a9, -12
    s32e    a2, a9, -8
    s32e    a3, a9, -4
    s32e    a4, a0, -32
    s32e    a5, a0, -28
    s32e    a6, a0, -24
    s32e    a7, a0, -20
    rfwo

    .section .wvec.uf8,"ax",@progbits
    .align 4
_WindowUnderflow8:
    l32e    a0, a9, -16
    l32e    a1, a9, -12
    l32e    a2, a9, -8
    l32e    a7, a1, -12
    l32e    a3, a9, -4
    l32e    a4, a7, -32
    l32e    a5, a7, -28
    l32e    a6, a7, -24
    l32e    a7, a7, -20
    rfwu

    .section .wvec.of12,"ax",@progbits
    .align 4
_WindowOverflow12:
    s32e    a0, a13, -16
    l32e    a0, a1, -12
    s32e    a1, a13, -12
    s32e    a2, a13, -8
    s32e    a3, a13, -4
    s32e    a4, a0, -48
    s32e    a5, a0, -44
    s32e    a6, a0, -40
    s32e    a7, a0, -36
    s32e    a8, a0, -32
    s32e    a9, a0, -28
    s32e    a10, a0, -24
    s32e    a11, a0, -20
    rfwo

    .section .wvec.uf12,"ax",@progbits
    .align 4
_WindowUnderflow12:
    l32e    a0, a13, -16
    l32e    a1, a13, -12
    l32e    a2, a13, -8
    l32e    a11, a1, -12
    l32e    a3, a13, -4
    l32e    a4, a11, -48
    l32e    a5, a11, -44
    l32e    a6, a11, -40
    l32e    a7, a11, -36
    l32e    a8, a11, -32
    l32e    a9, a11, -28
    l32e    a10, a11, -24
    l32e    a11, a11, -20
    rfwu
"#
);
