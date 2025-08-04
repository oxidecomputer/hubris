// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    i2c_config::{self, sensors},
    Ohms, PowerControllerConfig, PowerState,
};

pub(crate) const CONTROLLER_CONFIG_LEN: usize = 4;
pub(crate) static CONTROLLER_CONFIG: [PowerControllerConfig;
    CONTROLLER_CONFIG_LEN] = [
    adm1272_controller!(HotSwap, vbus_sled, A2, Ohms(0.001)),
    adm1272_controller!(Sys, vbus_sys, A2, Ohms(0.001)),
    rail_controller!(Sys, tps546B24A, v3p3_sys, A2),
    rail_controller!(Sys, tps546B24A, v1p0_sys, A2),
];

pub(crate) fn get_state() -> PowerState {
    PowerState::A2
}

pub(crate) struct State(());

impl State {
    pub(crate) fn init() -> Self {
        // Nothing to do here
        State(())
    }

    pub(crate) fn handle_timer_fired(
        &self,
        _devices: &[crate::Device],
        _state: PowerState,
        _packrat: &mut task_packrat_api::Packrat,
    ) {
    }
}

pub const HAS_RENDMP_BLACKBOX: bool = false;
