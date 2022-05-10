// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// We deliberately build every possible BSP here; the linker will strip them,
// and this prevents us from accidentally introducing breaking changes.
mod gimlet_a;
mod gimlet_b;

// Check that every BSP implements the BspT trait. This also prevents
// dead code warnings!
const _: () = {
    fn has_bsp<T: BspT>() {}
    fn assert_bsps() {
        has_bsp::<gimlet_a::Bsp>();
        has_bsp::<gimlet_b::Bsp>();
    }
};

pub(crate) trait BspT {
    fn new(i2c_task: userlib::TaskId) -> Self;

    /// Sensors which are monitored as part of the control loop
    fn inputs(&self) -> &[crate::control::InputChannel];

    /// Miscellaneous sensors, which are logged into the `sensor` task but
    /// do not affect the control loop
    fn misc_sensors(&self) -> &[crate::TemperatureSensor];

    /// Fan output group.  Each `ThermalControl` is limited to a single
    /// fan control IC, but can choose which fans to control.
    fn fans(
        &self,
    ) -> &[(drv_i2c_devices::max31790::Fan, task_sensor_api::SensorId)];

    /// Fan control IC
    fn fan_control(&self) -> &crate::FanControl;

    /// Returns a `u32` with a single bit set that corresponds to a power mode,
    /// which in turn determines which sensors are active.
    fn power_mode(&self) -> u32;
}

cfg_if::cfg_if! {
    if #[cfg(target_board = "gimlet-a")] {
        pub(crate) use gimlet_a::*;
    } else if #[cfg(target_board = "gimlet-b")] {
        pub(crate) use gimlet_b::*;
    } else {
        compiler_error!("No BSP for the given board");
    }
}
