// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common types and helpers for I2C Temperature Inputs

use crate::{
    bsp::PowerBitmask,
    control::{
        ChannelType, InputReadingOutcome, InputStatus, TemperatureReading,
        TimestampedTemperatureReading,
    },
};
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::{
    TempSensor, nvme_bmc::NvmeBmc, pct2075::Pct2075, sbtsi::Sbtsi,
    tmp117::Tmp117, tmp451::Tmp451, tse2004av::Tse2004Av,
};
use task_sensor_api::{NoData, SensorError, SensorId};
use task_thermal_api::{SensorReadError, ThermalProperties};
use userlib::{TaskId, sys_get_timer, units::Celsius};

/// Type containing all of our temperature sensor types, so we can store them
/// generically in an array.  These are all `I2cDevice`s, so functions on
/// this `enum` return an `drv_i2c_api::ResponseCode`.
#[allow(dead_code, clippy::upper_case_acronyms)]
pub enum Device {
    Tmp117,
    Tmp451(drv_i2c_devices::tmp451::Target),
    CPU,
    Dimm,
    U2,
    M2,
    LM75,
}

/// Represents a sensor in the system.
///
/// The sensor includes a device type, used to decide how to read it;
/// a free function that returns the raw `I2cDevice`, so that this can be
/// `const`); and the sensor ID, to post data to the `sensors` task.
pub struct TemperatureSensor {
    device: Device,
    builder: fn(TaskId) -> drv_i2c_api::I2cDevice,
    pub sensor_id: SensorId,
}

impl TemperatureSensor {
    pub const fn new(
        device: Device,
        builder: fn(TaskId) -> drv_i2c_api::I2cDevice,
        sensor_id: SensorId,
    ) -> Self {
        Self {
            device,
            builder,
            sensor_id,
        }
    }
    pub fn read_temp(
        &self,
        i2c_task: TaskId,
    ) -> Result<Celsius, SensorReadError> {
        let dev = (self.builder)(i2c_task);
        let t = match &self.device {
            Device::Tmp117 => Tmp117::new(&dev).read_temperature()?,
            Device::CPU => Sbtsi::new(&dev).read_temperature()?,
            Device::Tmp451(t) => Tmp451::new(&dev, *t).read_temperature()?,
            Device::Dimm => Tse2004Av::new(&dev).read_temperature()?,
            Device::U2 | Device::M2 => NvmeBmc::new(&dev).read_temperature()?,
            Device::LM75 => Pct2075::new(&dev).read_temperature()?,
        };
        Ok(t)
    }
}

/// InputChannelMetadata is the constant description portion of an InputChannel.
///
/// We split it off because InputChannel is mutable to contain the last state,
/// and if we left it inlined, then we would end up including all of this
/// metadata in RAM, despite never changing it! So instead, we break it off and
/// have InputChannel hold an `&'static` reference instead, so we only waste
/// a wee little pointer (4 bytes) to flash space in each InputChannel entry,
/// instead of (at the time of writing) 36 bytes, which for example in Cosmo
/// that has 14 input channels, is over 500 bytes of RAM!
pub(crate) struct InputChannelMetadata {
    /// Temperature sensor
    sensor: TemperatureSensor,

    /// Thermal properties of the associated component
    model: ThermalProperties,

    /// Mask with bits set based on the BSP's `power_mode` bits
    power_mode_mask: PowerBitmask,

    /// Channel type
    ty: ChannelType,
}

/// An `InputChannel` represents a temperature sensor associated with a
/// particular component in the system.
pub(crate) struct InputChannel {
    metadata: &'static InputChannelMetadata,
    last_reading: TemperatureReading,
}

impl InputChannelMetadata {
    pub(crate) const fn new(
        sensor: TemperatureSensor,
        model: ThermalProperties,
        power_mode_mask: PowerBitmask,
        ty: ChannelType,
    ) -> Self {
        Self {
            sensor,
            model,
            power_mode_mask,
            ty,
        }
    }
}

impl InputChannel {
    pub(crate) const fn new(metadata: &'static InputChannelMetadata) -> Self {
        Self {
            metadata,
            last_reading: TemperatureReading::NotYetQueried,
        }
    }

    pub(crate) fn has_been_queried(&self) -> bool {
        match self.last_reading {
            // If we haven't queried it, then no!
            TemperatureReading::NotYetQueried => false,

            // If we have queried it, and it is not relevant in the current
            // state, or removable and disconnected, or we've gotten data for
            // it at least once, it has been queried!
            TemperatureReading::Unpowered => true,
            TemperatureReading::Disconnected => true,
            TemperatureReading::ValidAtLeastOnce(..) => true,
        }
    }

    /// Get current stored status.
    ///
    /// Returns None if we do not have a reading stored.
    pub(crate) fn status(&self) -> Option<InputStatus<'_>> {
        let TemperatureReading::ValidAtLeastOnce(ref reading) =
            self.last_reading
        else {
            return None;
        };
        Some(InputStatus {
            sensor_id: self.metadata.sensor.sensor_id,
            reading,
            model: &self.metadata.model,
        })
    }

    pub(crate) fn reset_value(&mut self, mode: PowerBitmask) {
        if !mode.intersects(self.metadata.power_mode_mask) {
            self.last_reading = TemperatureReading::Unpowered;
        } else {
            self.last_reading = TemperatureReading::NotYetQueried;
        }
    }

    pub(crate) fn do_reading(
        &mut self,
        mode: PowerBitmask,
        i2c_task: &TaskId,
    ) -> InputReadingOutcome {
        let sensor_id = self.metadata.sensor.sensor_id;

        // If we're not supposed to be on, don't even ask.
        if !mode.intersects(self.metadata.power_mode_mask) {
            self.last_reading = TemperatureReading::Unpowered;
            return InputReadingOutcome::Unpowered { sensor_id };
        }

        match self.metadata.sensor.read_temp(*i2c_task) {
            Ok(value) => {
                let now = sys_get_timer().now;
                self.last_reading = TemperatureReading::ValidAtLeastOnce(
                    TimestampedTemperatureReading {
                        time_ms: now,
                        value,
                    },
                );
                InputReadingOutcome::Success {
                    sensor_id,
                    now,
                    value,
                }
            }
            Err(e) => {
                // This is mimicking the old state value logic for deciding if
                // we persist the data in `run_control`, that ONLY cleared the
                // persisted value if:
                //
                // - The sensor is not present AND removable
                // - The sensor is error prone
                //
                // Replicate that logic here, doing some type shenanigans
                // because we aren't round-tripping through the Sensor API
                // anymore.
                let se = SensorError::from(NoData::from(e));
                match (self.metadata.ty, se) {
                    (ChannelType::Removable, SensorError::NotPresent) => {
                        self.last_reading = TemperatureReading::Disconnected;
                    }
                    (ChannelType::RemovableAndErrorProne, _) => {
                        self.last_reading = TemperatureReading::Disconnected;
                    }
                    _ => {
                        // In all other cases, just leave whatever the last
                        // present value was so that the state estimation
                        // can continue estimating state.
                    }
                }

                // This logic comes from what was done in `read_sensors`,
                // which is only deciding whether it's worth logging about.
                // In either case, it will push NoData to the sensor api.
                //
                // This is *not* the same logic that is used above to decide
                // whether we clear the previous state or not, despite being
                // *very* similar!
                let removable = matches!(
                    self.metadata.ty,
                    ChannelType::Removable
                        | ChannelType::RemovableAndErrorProne
                );
                let removed =
                    e == SensorReadError::I2cError(ResponseCode::NoDevice);
                let unexpected_failure = !(removable && removed);
                if unexpected_failure {
                    InputReadingOutcome::UnacceptableMissing {
                        sensor_id,
                        err: e,
                    }
                } else {
                    InputReadingOutcome::AcceptableMissing { sensor_id, err: e }
                }
            }
        }
    }
}
