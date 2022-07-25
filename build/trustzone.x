  /* We only include this if we're building with TrustZone to avoid
     excessive alignment requirements */

   /* 32-byte alignment requirement for SAU */
  .nsc : ALIGN(32) {
    KEEP(*(.nsc));
    . = ALIGN(32);
  } > FLASH

  .tz_table : {
    KEEP(*(.tz_table));
  } > FLASH
