# LPC55 memory layout

The LPC55 has a first stage bootloader before booting hubris. This bootloader
runs in secure mode before transitioning to non-secure mode. The code currently
makes the assumption that hubris starts right at the end of the stage0
bootloader. This needs to be set appropriately in app.toml! The minimum
alignment for flash is 0x8000.

+----------------+  0x98000
|                |
|                |
|                |
|                |
|                |
|                |
|   Hubris       |
|                |
|                |
|                |
|                |
|                |
|                |
+----------------+  0x8000
|                |
|   stage0       |
|                |
+----------------+  0x0


