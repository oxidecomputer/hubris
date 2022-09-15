// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//
// Check that we have a BSP and that the implements the BspT trait. This also
// prevents dead code warnings!
//
const _: () = {
    fn has_bsp<T: BspT>() {}
    fn assert_bsps() {
        has_bsp::<Bsp>();
    }
};

pub(crate) trait BspT {
    fn new(i2c_task: userlib::TaskId) -> Self;

    /// Sensors which are monitored as part of the control loop
    fn inputs(&self) -> &[crate::control::InputChannel];

    /// Miscellaneous sensors, which are logged into the `sensor` task but
    /// do not affect the control loop
    fn misc_sensors(&self) -> &[crate::control::TemperatureSensor];

    /// Fan sensors
    fn fans(&self) -> &[task_sensor_api::SensorId];

    /// Fan control IC for a specified fan. Note that the input is a global
    /// fan index, and the BSP translates from this global index to a specific
    /// control and local fan index.
    fn fan_control(&self, fan: crate::Fan) -> crate::control::FanControl;

    /// All fan control ICs
    fn for_each_fctrl(&self, fctrl: impl FnMut(crate::control::FanControl));

    /// Returns a `u32` with a single bit set that corresponds to a power mode,
    /// which in turn determines which sensors are active.
    fn power_mode(&self) -> u32;
}

cfg_if::cfg_if! {
    if #[cfg(target_board = "gimlet-a")] {
        mod gimlet_a;
        pub(crate) use gimlet_a::*;
    } else if #[cfg(target_board = "gimlet-b")] {
        mod gimlet_b;
        pub(crate) use gimlet_b::*;
    } else if #[cfg(target_board = "sidecar-a")] {
        mod sidecar_a;
        pub(crate) use sidecar_a::*;
    } else {
        compile_error!("No BSP for the given board");
    }
}
