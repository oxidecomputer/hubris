// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Thermal loop
//!
//! This is a primordial thermal loop, which will ultimately reading temperature
//! sensors and control fan duty cycles to actively manage thermals.  Right now,
//! though it is merely reading every fan and temp sensor that it can find...
//!

#![no_std]
#![no_main]

mod bsp;
mod control;

use crate::{bsp::Bsp, control::ThermalControl};
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::max31790::Max31790;
pub use drv_i2c_devices::max31790::{Fan, PWMDuty};
use drv_i2c_devices::TempSensor;
use drv_i2c_devices::{
    sbtsi::Sbtsi, tmp117::Tmp117, tmp451::Tmp451, tse2004av::Tse2004Av,
};
use idol_runtime::{NotificationHandler, RequestError};
use task_thermal_api::ThermalError;
use userlib::units::*;
use userlib::*;

use task_sensor_api::{Sensor as SensorApi, SensorId};

task_slot!(I2C, i2c_driver);
task_slot!(SENSOR, sensor);

////////////////////////////////////////////////////////////////////////////////

enum Zone {
    East,
    Central,
    West,
}

enum Device {
    North(Zone, Tmp117),
    South(Zone, Tmp117),
    T6Nic(Tmp451),
    CPU(Sbtsi),
    Dimm(Tse2004Av),
}

struct TemperatureSensor {
    device: Device,
    id: SensorId,
}

impl TemperatureSensor {
    fn read_temp(&mut self) -> Result<Celsius, ResponseCode> {
        match &mut self.device {
            Device::North(_, dev) | Device::South(_, dev) => {
                dev.read_temperature().map_err(Into::into)
            }
            Device::CPU(dev) => dev.read_temperature().map_err(Into::into),
            Device::T6Nic(dev) => dev.read_temperature().map_err(Into::into),
            Device::Dimm(dev) => dev.read_temperature().map_err(Into::into),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

enum FanControl {
    Max31790(Max31790),
}

impl FanControl {
    fn set_pwm(&self, fan: Fan, pwm: PWMDuty) -> Result<(), ResponseCode> {
        match self {
            Self::Max31790(m) => m.set_pwm(fan, pwm),
        }
    }
    pub fn fan_rpm(&self, fan: Fan) -> Result<Rpm, ResponseCode> {
        match self {
            Self::Max31790(m) => m.fan_rpm(fan),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl<'a> {
    control: ThermalControl<'a>,
    deadline: u64,
}

const TIMER_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 1000;

impl<'a> idl::InOrderThermalImpl for ServerImpl<'a> {
    fn set_fan_pwm(
        &mut self,
        _: &RecvMessage,
        index: u8,
        pwm: u8,
    ) -> Result<(), RequestError<ThermalError>> {
        unimplemented!()
    }
}

impl<'a> NotificationHandler for ServerImpl<'a> {
    fn current_notification_mask(&self) -> u32 {
        TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        self.deadline += TIMER_INTERVAL;
        sys_set_timer(Some(self.deadline), TIMER_MASK);

        self.control.step();
    }
}

#[export_name = "main"]
fn main() -> ! {
    let i2c_task = I2C.get_task_id();
    let sensor_api = SensorApi::from(SENSOR.get_task_id());

    let mut bsp = Bsp::new(i2c_task);
    let control = bsp.controller(sensor_api);

    // This will put our timer in the past, and should immediately kick us.
    let deadline = sys_get_timer().now;
    sys_set_timer(Some(deadline), TIMER_MASK);

    let mut server = ServerImpl { control, deadline };
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::ThermalError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
