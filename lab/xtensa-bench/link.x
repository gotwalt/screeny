/* Two Xtensa facts shape this script.

   1. l32r can only reach literals at LOWER addresses, so each object's
      .literal must be emitted immediately before its .text -- that is what the
      interleaved wildcard does.
   2. A dc232b comes out of reset with only the static TLB entries live, so the
      only usable mapping is the 0xd0000000 window onto physical 0. Linking at
      a low virtual address faults straight into the vector at 0xd00003c0.

   qemu also ignores the ELF entry point and starts at the core's reset vector,
   so a four-instruction stub lives there and jumps to _start. It builds the
   target with movi/slli/addmi rather than l32r so it needs no literal pool. */
ENTRY(_start)
MEMORY {
  VEC : ORIGIN = 0xfe000000, LENGTH = 0x100
  RAM : ORIGIN = 0xd0002000, LENGTH = 0x00300000
}
SECTIONS {
  .reset : { KEEP(*(.literal.reset .text.reset)) } > VEC
  /* Register-window spill/fill vectors, 0x40 apart from the vector base. */
  .wvec.of4  0xd0000000 : { KEEP(*(.wvec.of4))  }
  .wvec.uf4  0xd0000040 : { KEEP(*(.wvec.uf4))  }
  .wvec.of8  0xd0000080 : { KEEP(*(.wvec.of8))  }
  .wvec.uf8  0xd00000c0 : { KEEP(*(.wvec.uf8))  }
  .wvec.of12 0xd0000100 : { KEEP(*(.wvec.of12)) }
  .wvec.uf12 0xd0000140 : { KEEP(*(.wvec.uf12)) }
  .text : ALIGN(4) {
    *(.literal.start .text.start)
    *(.literal .text .literal.* .text.*)
  } > RAM
  .rodata : ALIGN(4) { *(.rodata .rodata.*) } > RAM
  .data : ALIGN(4) { *(.data .data.*) } > RAM
  .bss (NOLOAD) : ALIGN(4) { *(.bss .bss.* COMMON) } > RAM
  /DISCARD/ : { *(.comment) *(.xt.*) *(.xtensa.*) }
}
