// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use userlib::*;

/* Sidecar-only for now!
 *
 * SP_QSPI_RESET_L     PF5     GPIO
 * SP_QSPI_CLK         PF10    QUADSPI_CLK
 * SP_QSPI_IO0 (SI)    PF8     QUADSPI_BK1_IO0
 * SP_QSPI_IO1 (SO)    PF9     QUADSPI_BK1_IO1
 * SP_QSPI_CS_L        PG6     QUADSPI_BK1_NCS (or GPIO?)
 * SP_QSPI_IO2 (*WP)   PF7     QUADSPI_BK1_IO2
 * SP_QSPI_IO3 (*HOLD) PF6     QUADSPI_BK1_IO3
 */

/* Gimlet uses SPI instead
 *
 * SP_TO_FLASH_SPI_HOLD_N  PB8     GPIO
 * SP_TO_FLASH_SPI_CLK     PB13    SPI2_SCK
 * SP_TO_FLASH_SPI_MISO    PB14    SPI2_MISO
 * SP_TO_FLASH_SPI_MOSI    PB15    SPI2_MOSI
 * SP_TO_FLASH_SPI_CS      PB12    SPI2_NSS (or GPIO)
 * SP_TO_FLASH_SPI_WP_N    PB9     GPIO
 *
 * (This is `local_flash` in `gimlet/rev-b.toml`)
 */

#[export_name = "main"]
fn main() -> ! {
    loop {
        // NOTE: you need to put code here before running this! Otherwise LLVM
        // will turn this into a single undefined instruction.
    }
}
