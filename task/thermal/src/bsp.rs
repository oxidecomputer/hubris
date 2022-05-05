// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// We deliberately build every possible BSP here; the linker will strip them,
// and this prevents us from accidentally introducing breaking changes.
mod gimlet_a;
mod gimlet_b;
mod sidecar_1;

use crate::control::{OutputFans, ThermalControl};
use task_sensor_api::Sensor as SensorApi;
use userlib::units::Celsius;

cfg_if::cfg_if! {
    if #[cfg(target_board = "gimlet-a")] {
        pub(crate) use gimlet_a::*;
    } else if #[cfg(target_board = "gimlet-b")] {
        pub(crate) use gimlet_b::*;
    } else if #[cfg(target_board = "sidecar-1")] {
        pub(crate) use sidecar_1::*;
    } else {
        compiler_error!("No BSP for the given board");
    }
}

// This `impl` block requires all of the `struct Bsp` to have the same internal
// structure. We could enforce this with a trait, but that doesn't make the
// code any more robust, since we're still conditionally importing based on
// target board.
impl Bsp {
    pub fn controller(&mut self, sensor_api: SensorApi) -> ThermalControl {
        ThermalControl {
            inputs: &mut self.inputs,
            outputs: OutputFans {
                fctrl: &self.fctrl,
                fans: &self.fans,
            },
            misc_sensors: &mut self.misc_sensors,
            sensor_api,
            hysteresis: Celsius(2.0f32),
            target_margin: Celsius(2.0f32),
            target_pwm: 100,
        }
    }
}
