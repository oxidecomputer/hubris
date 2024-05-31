// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A crate for managing the power supplies on the Medusa board

use crate::*;
use drv_stm32xx_sys_api as sys_api;
use sys_api::{OutputType, Port, Pull, Speed, Sys};

task_slot!(SYS, sys);

pub struct PowerRail {
    enable: sys_api::PinSet,
    power_good: sys_api::PinSet,
}

impl PowerRail {
    pub fn new(enable: sys_api::PinSet, power_good: sys_api::PinSet) -> Self {
        let sys = Sys::from(SYS.get_task_id());

        sys.gpio_configure_output(
            enable,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        );

        sys.gpio_configure_input(power_good, Pull::None);

        Self { enable, power_good }
    }
    pub fn set_enable(&self, enabled: bool) {
        let sys = Sys::from(SYS.get_task_id());
        ringbuf_entry!(Trace::FrontIOBoardPowerEnable(enabled));
        sys.gpio_set_to(self.enable, enabled)
    }

    pub fn power_good(&self) -> bool {
        let sys = Sys::from(SYS.get_task_id());
        sys.gpio_read(self.power_good) != 0
    }
}

pub struct PowerControl {
    pub v12_qsfp_out: PowerRail,
    pub v1p0_mgmt: PowerRail,
    pub v1p2_mgmt: PowerRail,
    pub v2p5_mgmt: PowerRail,
    pub v1p0_phy: PowerRail,
    pub v2p5_phy: PowerRail,
}

impl PowerControl {
    pub fn new() -> Self {
        let v12_qsfp_out = PowerRail::new(Port::J.pin(2), Port::J.pin(1));
        let v1p0_mgmt = PowerRail::new(Port::J.pin(4), Port::J.pin(3));
        let v1p2_mgmt = PowerRail::new(Port::J.pin(6), Port::J.pin(5));
        let v2p5_mgmt = PowerRail::new(Port::J.pin(8), Port::J.pin(7));
        // The PHY rails are generated from the same LDO which shares an enable pin
        let v1p0_phy = PowerRail::new(Port::J.pin(10), Port::J.pin(11));
        let v2p5_phy = PowerRail::new(Port::J.pin(10), Port::J.pin(12));

        Self {
            v12_qsfp_out,
            v1p0_mgmt,
            v1p2_mgmt,
            v2p5_mgmt,
            v1p0_phy,
            v2p5_phy,
        }
    }
}
