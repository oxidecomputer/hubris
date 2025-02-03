// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    i2c_config::{self, sensors},
    Ohms, PowerControllerConfig, PowerState,
};

pub(crate) const CONTROLLER_CONFIG_LEN: usize = 1;
pub(crate) static CONTROLLER_CONFIG: [PowerControllerConfig;
    CONTROLLER_CONFIG_LEN] = [lm5066_controller!(
    HotSwap,
    lm5066_evl_vout,
    A2,
    Ohms(0.003),
    drv_i2c_devices::lm5066::CurrentLimitStrap::VDD
)];

pub(crate) fn get_state() -> PowerState {
    PowerState::A2
}

pub(crate) struct State(());

impl State {
    pub(crate) fn init() -> Self {
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
