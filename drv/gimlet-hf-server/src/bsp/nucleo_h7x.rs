// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Config;
use drv_stm32h7_qspi::Qspi;
use drv_stm32xx_sys_api as sys_api;

#[allow(dead_code)]
pub(crate) fn init(qspi: &Qspi, sys: &sys_api::Sys) -> Config {
    // Nucleo-h743zi2/h753zi pin mappings
    // These development boards are often wired by hand.
    // Although there are several choices for pin assignment,
    // the CN10 connector on the board has a marked "QSPI" block
    // of pins. Use those. Use two pull-up resistors and a
    // decoupling capacitor if needed.
    //
    // CNxx- Pin   MT25QL256xxx
    // pin   Fn    Pin           Signal   Notes
    // ----- ---   ------------, -------, ------
    // 10-07 PF4,  3,            RESET#,  10K ohm to Vcc
    // 10-09 PF5,  ---           nc,
    // 10-11 PF6,  ---           nc,
    // 10-13 PG6,  7,            CS#,     10K ohm to Vcc
    // 10-15 PB2,  16,           CLK,
    // 10-17 GND,  10,           GND,
    // 10-19 PD13, 1,            IO3,
    // 10-21 PD12, 8,            IO1,
    // 10-23 PD11, 15,           IO0,
    // 10-25 PE2,  9,            IO2,
    // 10-27 GND,  ---           nc,
    // 10-29 PA0,  ---           nc,
    // 10-31 PB0,  ---           nc,
    // 10-33 PE0,  ---           nc,
    //
    // 08-07 3V3,  2,            Vcc,     100nF to GND
    let clock = 8; // 200MHz kernel / 8 = 25MHz clock
    qspi.configure(
        clock, 25, // 2**25 = 32MiB = 256Mib
    );
    // Nucleo-144 pin mapping
    // PB2 SP_QSPI1_CLK
    // PD11 SP_QSPI1_IO0
    // PD12 SP_QSPI1_IO1
    // PD13 SP_QSPI1_IO3
    // PE2 SP_QSPI1_IO2
    //
    // PG6 SP_QSPI1_CS
    //
    sys.gpio_configure_alternate(
        sys_api::Port::B.pin(2),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
        sys_api::Alternate::AF9,
    );
    sys.gpio_configure_alternate(
        sys_api::Port::D.pin(11).and_pin(12).and_pin(13),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
        sys_api::Alternate::AF9,
    );
    sys.gpio_configure_alternate(
        sys_api::Port::E.pin(2),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
        sys_api::Alternate::AF9,
    );
    sys.gpio_configure_alternate(
        sys_api::Port::G.pin(6),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    );

    Config {
        sp_host_mux_select: sys_api::Port::F.pin(5),
        reset: sys_api::Port::F.pin(4),
        flash_dev_select: None,
        clock,
    }
}
