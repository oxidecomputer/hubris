// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    i2c_config::{self, sensors},
    DeviceType, Ohms, PowerControllerConfig, PowerState,
};

pub(crate) const CONTROLLER_CONFIG: [PowerControllerConfig; 1] = [
    // The DC2024 has 10 3mΩ current sense resistors in parallel (5 on each
    // channel), given a total current sense resistance of 300µΩ
    ltc4282_controller!(HotSwapQSFP, v12_out_100a, A2, Ohms(0.003 / 10.0)),
];

pub(crate) fn get_state() -> PowerState {
    PowerState::A2
}

pub fn preinit() {
    // Nothing to do here
}
