// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Config;

use drv_stm32h7_qspi::Qspi;
use drv_stm32xx_sys_api as sys_api;

#[allow(dead_code)]
pub(crate) fn init(qspi: &Qspi, sys: &sys_api::Sys) -> Config {
    // PF4 HOST_ACCESS
    // PF5 RESET
    // PF6:AF9 IO3
    // PF7:AF9 IO2
    // PF8:AF10 IO0
    // PF9:AF10 IO1
    // PF10:AF9 CLK
    // PB6:AF10 CS
    let clock = 200 / 25; // 200MHz kernel clock / $x MHz SPI clock = divisor
    qspi.configure(
        clock, 25, // 2**25 = 32MiB = 256Mib
    );
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(6).and_pin(7).and_pin(10),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
        sys_api::Alternate::AF9,
    );
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(8).and_pin(9),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    );
    sys.gpio_configure_alternate(
        sys_api::Port::B.pin(6),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    );

    Config {
        sp_host_mux_select: sys_api::Port::F.pin(4),
        reset: sys_api::Port::F.pin(5),
        flash_dev_select: None,
        clock,
    }
}
