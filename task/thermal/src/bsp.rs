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

pub(crate) struct BspData<'a> {
    pub inputs: &'a mut [crate::control::InputChannel],
    pub misc_sensors: &'a mut [crate::TemperatureSensor],
    pub fans:
        &'a [(drv_i2c_devices::max31790::Fan, task_sensor_api::SensorId)],
    pub fctrl: crate::FanControl,
}

pub(crate) trait BspT {
    fn data(&mut self) -> BspData;
    fn new(i2c_task: userlib::TaskId) -> Self;
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
