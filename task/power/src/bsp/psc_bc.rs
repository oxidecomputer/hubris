// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    i2c_config::{self, sensors},
    PowerControllerConfig, PowerState,
};

pub(crate) const CONTROLLER_CONFIG_LEN: usize = 12;
pub(crate) static CONTROLLER_CONFIG: [PowerControllerConfig;
    CONTROLLER_CONFIG_LEN] = [
    mwocp68_controller!(PowerShelf, v54_psu0, A2),
    mwocp68_controller!(PowerShelf, v12_psu0, A2),
    mwocp68_controller!(PowerShelf, v54_psu1, A2),
    mwocp68_controller!(PowerShelf, v12_psu1, A2),
    mwocp68_controller!(PowerShelf, v54_psu2, A2),
    mwocp68_controller!(PowerShelf, v12_psu2, A2),
    mwocp68_controller!(PowerShelf, v54_psu3, A2),
    mwocp68_controller!(PowerShelf, v12_psu3, A2),
    mwocp68_controller!(PowerShelf, v54_psu4, A2),
    mwocp68_controller!(PowerShelf, v12_psu4, A2),
    mwocp68_controller!(PowerShelf, v54_psu5, A2),
    mwocp68_controller!(PowerShelf, v12_psu5, A2),
];

pub(crate) fn get_state() -> PowerState {
    PowerState::A2
}

pub(crate) struct State(());

impl State {
    pub(crate) fn init() -> Self {
        // Before talking to the power shelves, we have to enable an I2C buffer
        userlib::task_slot!(SYS, sys);
        use drv_stm32xx_sys_api::*;

        let sys_task = SYS.get_task_id();
        let sys = Sys::from(sys_task);

        let i2c_en = Port::E.pin(15); // SP_TO_BP_I2C_EN
        sys.gpio_set(i2c_en);
        sys.gpio_configure_output(
            i2c_en,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        );

        State(())
    }

    pub(crate) fn handle_timer_fired(
        &self,
        _devices: &[crate::Device],
        _state: PowerState,
    ) {
    }
}

pub const HAS_RENDMP_BLACKBOX: bool = false;
