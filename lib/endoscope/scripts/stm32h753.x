
/* Provides information about the memory layout of the device */
MEMORY
{
FLASH (rwx) : ORIGIN = 0x08000000, LENGTH = 0x00100000
/* RAM is artificially reduced to catch program becoming too large */
/* RAM (rwx) : ORIGIN   = 0x24000000, LENGTH = 0x00004000 */
/* STACK (rw) : ORIGIN  = 0x24004000, LENGTH = 0x00001000 */
/* Use Instruction Tightly Coupled Memory (ITCM) for RAM */
RAM (rwx) : ORIGIN   = 0x00000000, LENGTH = 0x00010000
/* Use Data Tightly Coupled Memory (ITCM) for STACK */
STACK (rw) : ORIGIN   = 0x20000000, LENGTH = 0x00020000
}

__eheap = ORIGIN(RAM) + LENGTH(RAM);
_stack_base = ORIGIN(STACK);
_stack_start = ORIGIN(STACK) + LENGTH(STACK);

FLASH_BASE = 0x08000000;
FLASH_SIZE = 0x00100000;

INCLUDE "endoscope.x"
