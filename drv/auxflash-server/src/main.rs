// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use userlib::*;

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

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_stm32h7_qspi::Qspi;
use drv_stm32xx_sys_api as sys_api;

task_slot!(SYS, sys);

const QSPI_IRQ: u32 = 1;

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.enable_clock(sys_api::Peripheral::QuadSpi);
    sys.leave_reset(sys_api::Peripheral::QuadSpi);

    let reg = unsafe { &*device::QUADSPI::ptr() };
    let qspi = Qspi::new(reg, QSPI_IRQ);

    let clock = 5; // 200MHz kernel / 5 = 40MHz clock
    qspi.configure(clock, 24); // 2**24 = 16MiB = 128Mib

    // Sidecar-only for now!
    //
    // This is mostly copied from `gimlet-hf-server`, with a few pin adjustments
    //
    // SP_QSPI_RESET_L     PF5     GPIO
    // SP_QSPI_CLK         PF10    QUADSPI_CLK
    // SP_QSPI_IO0 (SI)    PF8     QUADSPI_BK1_IO0
    // SP_QSPI_IO1 (SO)    PF9     QUADSPI_BK1_IO1
    // SP_QSPI_CS_L        PG6     QUADSPI_BK1_NCS (or GPIO?)
    // SP_QSPI_IO2 (*WP)   PF7     QUADSPI_BK1_IO2
    // SP_QSPI_IO3 (*HOLD) PF6     QUADSPI_BK1_IO3
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(6).and_pin(7).and_pin(10),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF9,
    )
    .unwrap();
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(8).and_pin(9),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    )
    .unwrap();
    sys.gpio_configure_alternate(
        sys_api::Port::G.pin(6),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    )
    .unwrap();

    let qspi_reset = sys_api::Port::F.pin(5);
    sys.gpio_reset(qspi_reset).unwrap();
    sys.gpio_configure_output(
        qspi_reset,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    )
    .unwrap();

    // TODO: The best clock frequency to use can vary based on the flash
    // part, the command used, and signal integrity limits of the board.

    // Ensure hold time for reset in case we just restarted.
    // TODO look up actual hold time requirement
    hl::sleep_for(1);

    // Release reset and let it stabilize.
    sys.gpio_set(qspi_reset).unwrap();
    hl::sleep_for(10);

    // TODO: check the ID and make sure it's what we expect

    loop {
        hl::sleep_for(1000);
    }
}
