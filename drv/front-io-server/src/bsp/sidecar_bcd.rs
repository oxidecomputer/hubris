// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_fpga_api::{FpgaError, FpgaUserDesign};
use drv_fpga_user_api::power_rail::{PowerRailStatus, RawPowerRailState};
use drv_front_io_api::FrontIOError;
use drv_sidecar_mainboard_controller::{Addr, MainboardController, Reg};
use drv_stm32xx_sys_api as sys_api;
use userlib::task_slot;

task_slot!(SYS, sys);
task_slot!(MAINBOARD, mainboard);

pub struct Bsp {
    fpga: FpgaUserDesign,
    sys: sys_api::Sys,
    pg_pin: sys_api::PinSet,
}

impl Bsp {
    pub fn new() -> Result<Self, FrontIOError> {
        let sys = sys_api::Sys::from(SYS.get_task_id());
        let pg_pin = sys_api::Port::F.pin(12); // QSFP_2_SP_A2_PG
        sys.gpio_configure_input(pg_pin, sys_api::Pull::None);

        Ok(Self {
            fpga: FpgaUserDesign::new(
                MAINBOARD.get_task_id(),
                MainboardController::DEVICE_INDEX,
            ),
            sys,
            pg_pin,
        })
    }

    #[inline]
    fn raw_state(&self) -> Result<RawPowerRailState, FpgaError> {
        self.fpga.read(Addr::FRONT_IO_STATE)
    }

    #[inline]
    pub fn power_rail_status(&self) -> Result<PowerRailStatus, FpgaError> {
        PowerRailStatus::try_from(self.raw_state()?)
    }

    pub fn power_good(&self) -> bool {
        self.sys.gpio_read(self.pg_pin) != 0
    }

    pub fn set_power_enable(&self, enable: bool) -> Result<(), FrontIOError> {
        self.fpga
            .write(
                enable.into(),
                Addr::FRONT_IO_STATE,
                Reg::FRONT_IO_STATE::ENABLE,
            )
            .map_err(FrontIOError::from)
    }
}
