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

    let qspi_reset = sys_api::Port::F.pin(5);

    sys.gpio_reset(qspi_reset);
    sys.gpio_configure_output(
        qspi_reset,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );

    Config { reset: qspi_reset }
}
