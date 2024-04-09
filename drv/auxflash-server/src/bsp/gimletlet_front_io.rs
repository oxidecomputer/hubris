// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Config;
use drv_stm32h7_qspi::Qspi;
use drv_stm32xx_sys_api as sys_api;

use drv_auxflash_api::{SLOT_COUNT, SLOT_SIZE};

pub(crate) fn init(qspi: &Qspi, sys: &sys_api::Sys) -> Config {
    let clock = 5; // 200MHz kernel / 5 = 40MHz clock
    const MEMORY_SIZE: usize = SLOT_COUNT as usize * SLOT_SIZE;
    assert!(MEMORY_SIZE.is_power_of_two());
    let memory_size_log2 = MEMORY_SIZE.trailing_zeros().try_into().unwrap();
    qspi.configure(clock, memory_size_log2);

    // Sidecar pin mapping
    //
    // This is mostly copied from `gimlet-hf-server`, with a few pin adjustments
    //
    // QSPI_XTRA7       PF4     GPIO
    // QSPI_CLK         PF10    QUADSPI_CLK
    // QSPI_IO0 (SI)    PF8     QUADSPI_BK1_IO0
    // QSPI_IO1 (SO)    PF9     QUADSPI_BK1_IO1
    // QSPI_CS (_L)     PG6     QUADSPI_BK1_NCS (or GPIO?)
    // QSPI_IO2 (*WP)   PF7     QUADSPI_BK1_IO2
    // QSPI_IO3 (*HOLD) PF6     QUADSPI_BK1_IO3
    //
    // Bonus pin to work with the spimux board:
    // QSPI_XTRA10      PF5     GPIO
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(6).and_pin(7).and_pin(10),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF9,
    );
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(8).and_pin(9),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    );
    sys.gpio_configure_alternate(
        sys_api::Port::G.pin(6),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    );

    // QSPI_XTRA7 -> SP_FLASH_RESET_L (spimux)
    let qspi_reset = sys_api::Port::F.pin(4);
    sys.gpio_reset(qspi_reset);
    sys.gpio_configure_output(
        qspi_reset,
        sys_api::OutputType::PushPull,
        sys_api::Speed::High,
        sys_api::Pull::None,
    );

    // QSPI_XTRA10 -> FLASH_MUX_SELECT (spimux)
    let mux_select = sys_api::Port::F.pin(5);
    sys.gpio_reset(mux_select);
    sys.gpio_configure_output(
        mux_select,
        sys_api::OutputType::PushPull,
        sys_api::Speed::High,
        sys_api::Pull::None,
    );

    // We drive this low so the mux always selects the SP
    sys.gpio_reset(mux_select);

    Config { reset: qspi_reset }
}
