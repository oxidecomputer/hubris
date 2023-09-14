// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Sensor task.

#![no_std]

use userlib::*;

pub use task_sensor_types::*;

impl Sensor {
    /// Post the given data with a timestamp of now
    #[inline]
    pub fn post_now(
        &self,
        id: SensorId,
        value: f32,
    ) -> Result<(), SensorApiError> {
        self.post(id, value, sys_get_timer().now)
    }

    /// Post the given `NoData` error with a timestamp of now
    #[inline]
    pub fn nodata_now(
        &self,
        id: SensorId,
        nodata: NoData,
    ) -> Result<(), SensorApiError> {
        self.nodata(id, nodata, sys_get_timer().now)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
include!(concat!(env!("OUT_DIR"), "/sensor_config.rs"));
