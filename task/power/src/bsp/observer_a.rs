// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{PowerControllerConfig, PowerState};

// TODO add rectifiers (once we have their SMBus spec)
pub(crate) const CONTROLLER_CONFIG_LEN: usize = 0;
pub(crate) static CONTROLLER_CONFIG: [PowerControllerConfig;
    CONTROLLER_CONFIG_LEN] = [];

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

        let i2c_en = Port::H.pin(10); // SP_TO_POWER_SHELF_PMBUS_BUFFER_EN
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
