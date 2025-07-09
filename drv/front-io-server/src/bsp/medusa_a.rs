// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_fpga_api::FpgaError;
use drv_fpga_user_api::power_rail::PowerRailStatus;
use drv_front_io_api::FrontIOError;
use drv_stm32xx_sys_api as sys_api;
use userlib::task_slot;

task_slot!(SYS, sys);

pub struct Bsp {
    sys: sys_api::Sys,
    en_pin: sys_api::PinSet,
    pg_pin: sys_api::PinSet,
}

impl Bsp {
    pub fn new() -> Result<Self, FrontIOError> {
        let sys = sys_api::Sys::from(SYS.get_task_id());
        let en_pin = sys_api::Port::J.pin(2); // SP_TO_V12_QSFP_OUT_EN
        let pg_pin = sys_api::Port::J.pin(1); // OUTPUT_HS_TO_SP_PG

        sys.gpio_configure_output(
            en_pin,
            sys_api::OutputType::PushPull,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );

        sys.gpio_configure_input(pg_pin, sys_api::Pull::None);

        Ok(Self {
            sys,
            en_pin,
            pg_pin,
        })
    }

    // PowerRailStatus is a FPGA concept baked into the Sidecar Mainboard design. Since we don't
    // have an FPGA here, we simplify things to just being enabled or disabled
    pub fn power_rail_status(&self) -> Result<PowerRailStatus, FpgaError> {
        let status = if self.sys.gpio_read(self.en_pin) != 0 {
            PowerRailStatus::Enabled
        } else {
            PowerRailStatus::Disabled
        };
        Ok(status)
    }

    pub fn power_good(&self) -> bool {
        self.sys.gpio_read(self.pg_pin) != 0
    }

    pub fn set_power_enable(&self, enabled: bool) -> Result<(), FrontIOError> {
        self.sys.gpio_set_to(self.en_pin, enabled);
        Ok(())
    }
}
