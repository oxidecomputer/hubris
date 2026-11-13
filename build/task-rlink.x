/* Linker script for a relocatable task
 * This should be used with the `-r` linker flag */
ENTRY(_start);

SECTIONS
{
  /* ### .text */
  .text : {
    _stext = .;
    *(.text.start*); /* try and pull start symbol to beginning */
    *(.text .text.*);
    . = ALIGN(4);
    __etext = .;
  }

  /* ### .rodata */
  .rodata : ALIGN(4)
  {
    *(.rodata .rodata.*);

    /* 4-byte align the end (VMA) of this section.
       This is required by LLD to ensure the LMA of the following .data
       section will have the correct alignment. */
    . = ALIGN(4);
    __erodata = .;
  }

  /*
   * Sections in RAM
   *
   * NOTE: the userlib runtime assumes that these sections
   * are 4-byte aligned and padded to 4-byte boundaries.
   */
  .data : ALIGN(4) {
    . = ALIGN(4);
    __sdata = .;
    *(.data .data.*);
    . = ALIGN(4); /* 4-byte align the end (VMA) of this section */
    __edata = .;
  }

  .bss (NOLOAD) : ALIGN(4) {
    . = ALIGN(4);
    __sbss = .;
    *(.bss .bss.*);
    . = ALIGN(4); /* 4-byte align the end (VMA) of this section */
    __ebss = .;
  }

  .uninit (NOLOAD) : ALIGN(4) {
    . = ALIGN(4);
    *(.uninit .uninit.*);
    . = ALIGN(4);
    /* Place the heap right after `.uninit` */
    __sheap = .;
  }

  /* ## .caboose_pos_table */
  /* Table of CaboosePos instances and their names. Used to record caboose
     position during packaging. */
  .caboose_pos_table (INFO) : {
    . = .;
    KEEP(*(.caboose_pos_table));
  }

  /* ## .idolatry */
  .idolatry (INFO) : {
    . = .;
    KEEP(*(.idolatry));
  }

  /* ## .nprpc_schema */
  /* Exported `temperance` interface schemas, see `temperance_net::expose_interface!`. */
  .nprpc_schema (INFO) : {
    . = .;
    KEEP(*(.nprpc_schema));
  }

  /* ## .temperance_ipc_schema */
  /* TODO: Add comment */
  .temperance_ipc_schema (INFO) : {
    . = .;
    KEEP(*(.temperance_ipc_schema));
  }

  /* ## Discarded sections */
  /DISCARD/ :
  {
    /* Unused exception related info that only wastes space */
    *(.ARM.exidx);
    *(.ARM.exidx.*);
    *(.ARM.extab.*);
    *(.got .got.*);
  }
}
