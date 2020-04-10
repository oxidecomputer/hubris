MEMORY
{
  /* NOTE K = KiBi = 1024 bytes */
  FLASH  (rx)  : ORIGIN = 0x08000000, LENGTH = 512K 
  RAM    (rwx) : ORIGIN = 0x20000000, LENGTH = 112K
  CCM    (rw)  : ORIGIN = 0x10000000, LENGTH =  64K
  SRAM16 (rwx) : ORIGIN = 0x2001c000, LENGTH =  16K
}

_stack_start = ORIGIN(RAM) + LENGTH(RAM);
