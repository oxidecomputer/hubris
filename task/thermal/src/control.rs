// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{bsp::BspT, Fan, ThermalError, Trace};
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::max31790::{I2cWatchdog, Max31790};
use drv_i2c_devices::TempSensor;
use drv_i2c_devices::{
    sbtsi::Sbtsi, tmp117::Tmp117, tmp451::Tmp451, tse2004av::Tse2004Av,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_sensor_api::{Sensor as SensorApi, SensorId};
use userlib::units::{Celsius, PWMDuty, Rpm};

////////////////////////////////////////////////////////////////////////////////

/// Type containing all of our temperature sensor types, so we can store them
/// generically in an array.  These are all `I2cDevice`s, so functions on
/// this `enum` return an `drv_i2c_api::ResponseCode`.
#[allow(dead_code, clippy::upper_case_acronyms)]
pub enum Device {
    Tmp117(Tmp117),
    Tmp451(Tmp451),
    CPU(Sbtsi),
    Dimm(Tse2004Av),
}

/// Represents a sensor and its associated `SensorId`, used when posting data
/// to the `sensors` task.
pub struct TemperatureSensor {
    device: Device,
    id: SensorId,
}

impl TemperatureSensor {
    pub fn new(device: Device, id: SensorId) -> Self {
        Self { device, id }
    }
    fn read_temp(&self) -> Result<Celsius, ResponseCode> {
        let t = match &self.device {
            Device::Tmp117(dev) => dev.read_temperature()?,
            Device::CPU(dev) => dev.read_temperature()?,
            Device::Tmp451(dev) => dev.read_temperature()?,
            Device::Dimm(dev) => dev.read_temperature()?,
        };
        Ok(t)
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Enum representing any of our fan controller types, bound to one of their
/// fans.  This lets us handle heterogeneous fan controller ICs generically
/// (although there's only one at the moment)
pub enum FanControl<'a> {
    Max31790(&'a Max31790, drv_i2c_devices::max31790::Fan),
}

impl<'a> FanControl<'a> {
    fn set_pwm(&self, pwm: PWMDuty) -> Result<(), ResponseCode> {
        match self {
            Self::Max31790(m, fan) => m.set_pwm(*fan, pwm),
        }
    }

    pub fn fan_rpm(&self) -> Result<Rpm, ResponseCode> {
        match self {
            Self::Max31790(m, fan) => m.fan_rpm(*fan),
        }
    }

    pub fn set_watchdog(&self, wd: I2cWatchdog) -> Result<(), ResponseCode> {
        match self {
            Self::Max31790(m, _fan) => m.set_watchdog(wd),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub(crate) struct InputChannel {
    /// Temperature sensor
    sensor: TemperatureSensor,

    /// Maximum temperature for this part
    max_temp: Celsius,

    /// Mask with bits set based on the Bsp's `power_mode` bits
    power_mode_mask: u32,

    /// If we get `NoDevice` for a removable device, ignore it
    removable: bool,
}

impl InputChannel {
    pub fn new(
        sensor: TemperatureSensor,
        max_temp: Celsius,
        power_mode_mask: u32,
        removable: bool,
    ) -> Self {
        Self {
            sensor,
            max_temp,
            power_mode_mask,
            removable,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// The thermal control loop.
///
/// This object uses slices of sensors and fans, which must be owned
/// elsewhere; the standard pattern is to create static arrays in a
/// `struct Bsp` which is conditionally included based on board name.
///
/// ```
///         |
///   HOT   |   Fan speed increases quickly
///         |
///         0------ Component margin + target_margin --
///         |
///         |   Fan speed increases slowly
///         |
///         ------- slow_band -------------------------
///         |
///         |   Fan speed remains constant
///         |
///         ------- dead_band -------------------------
///         |
///   COLD  |   Fan speed decreases slowly
///         |
/// ```
///
pub(crate) struct ThermalControl<'a, B> {
    /// Reference to board-specific parameters
    bsp: &'a B,

    /// Task to which we should post sensor data updates
    sensor_api: SensorApi,

    /// Dead band between increasing and decreasing fan speed
    dead_band: Celsius,

    /// Band where fan speed increases slowly
    slow_band: Celsius,

    /// Target temperature margin. This must be >= 0; as it increases, parts
    /// are kept cooler than their max temperature ratings.
    target_margin: Celsius,

    /// Commanded PWM value (0-100) for every output channel
    target_pwm: u8,

    read_failed_count: u32,
    post_failed_count: u32,
}

impl<'a, B: BspT> ThermalControl<'a, B> {
    /// Constructs a new `ThermalControl` based on a `struct Bsp`. This
    /// requires that every BSP has the same internal structure,
    pub fn new(bsp: &'a B, sensor_api: SensorApi) -> Self {
        let dead_band = Celsius(2.0f32);
        let slow_band = Celsius(1.0f32);
        assert!(dead_band.0 > slow_band.0);
        Self {
            bsp,
            sensor_api,
            dead_band,
            slow_band,
            target_margin: Celsius(2.0f32),
            target_pwm: 100,
            read_failed_count: 0,
            post_failed_count: 0,
        }
    }

    /// Reads all temperature and fan RPM sensors, posting their results
    /// to the sensors task API. Returns the worst margin; positive means
    /// all parts are happily below their max temperatures, while negative
    /// means someone is overheating.
    ///
    /// Records failed reads to non-controlled sensors and failed posts to the
    /// sensors task in `self.read_failed_count` and `self.post_failed_count`
    /// respectively.
    ///
    /// Returns an error if any of the *controlled* sensors fails to read.
    /// Note that monitored sensors may fail to read and the sensor post
    /// may fail without this returning an error; an error means that the
    /// integrity of the control loop is threatened.
    pub fn read_sensors(&mut self) -> Result<Option<f32>, ResponseCode> {
        // Read fan data and log it to the sensors task
        for (index, sensor_id) in self.bsp.fans().iter().enumerate() {
            let post_result =
                match self.bsp.fan_control(Fan::from(index)).fan_rpm() {
                    Ok(reading) => {
                        self.sensor_api.post(*sensor_id, reading.0.into())
                    }
                    Err(e) => {
                        ringbuf_entry!(Trace::FanReadFailed(index, e));
                        self.sensor_api.nodata(*sensor_id, e.into())
                    }
                };
            if post_result.is_err() {
                self.post_failed_count = self.post_failed_count.wrapping_add(1);
            }
        }

        // Read miscellaneous temperature data and log it to the sensors task
        for (i, s) in self.bsp.misc_sensors().iter().enumerate() {
            let post_result = match s.read_temp() {
                Ok(v) => self.sensor_api.post(s.id, v.0),
                Err(e) => {
                    ringbuf_entry!(Trace::MiscReadFailed(i, e));
                    self.read_failed_count =
                        self.read_failed_count.wrapping_add(1);
                    self.sensor_api.nodata(s.id, e.into())
                }
            };
            if post_result.is_err() {
                self.post_failed_count = self.post_failed_count.wrapping_add(1);
            }
        }

        // Remember, positive margin means that all parts are happily below
        // their max temperature; negative means someone is overheating.
        let mut worst_margin = None;
        let mut last_err = Ok(());
        let power_mode = self.bsp.power_mode();
        for (i, s) in self.bsp.inputs().iter().enumerate() {
            let post_result = match s.sensor.read_temp() {
                Ok(v) => {
                    if (s.power_mode_mask & power_mode) != 0 {
                        let margin = s.max_temp.0 - v.0;
                        worst_margin = Some(match worst_margin {
                            Some(m) => margin.min(m),
                            None => margin,
                        });
                    }
                    self.sensor_api.post(s.sensor.id, v.0)
                }
                Err(e) => {
                    // Ignore errors if
                    // a) this sensor shouldn't be on in this power mode, or
                    // b) the sensor is removable and not present
                    if (s.power_mode_mask & power_mode) != 0
                        && !(s.removable && e == ResponseCode::NoDevice)
                    {
                        last_err = Err(e);
                        ringbuf_entry!(Trace::SensorReadFailed(i, e));
                    }
                    self.sensor_api.nodata(s.sensor.id, e.into())
                }
            };
            if post_result.is_err() {
                self.post_failed_count = self.post_failed_count.wrapping_add(1);
            }
        }

        // Prioritize returning errors, because they indicate that something is
        // wrong with the sensors that are critical to the control loop.
        last_err?;

        Ok(worst_margin)
    }

    /// An extremely simple thermal control loop.
    ///
    /// Returns an error if the control loop failed to read critical sensors;
    /// the caller should set us to some kind of fail-safe mode if this
    /// occurs.
    pub fn run_control(&mut self) -> Result<(), ThermalError> {
        let mut r = self
            .read_sensors()
            .map_err(|_| ThermalError::DeviceError)?
            .ok_or(ThermalError::NoReading)?;

        r -= self.target_margin.0;
        if r < 0.0f32 {
            self.target_pwm = (self.target_pwm + 5).min(100);
        } else if r < self.slow_band.0 {
            self.target_pwm = (self.target_pwm + 1).min(100);
        } else if r < self.dead_band.0 {
            // No change
        } else {
            self.target_pwm = self.target_pwm.saturating_sub(1);
        }

        // Send the new RPM to all of our fans
        ringbuf_entry!(Trace::ControlPwm(self.target_pwm));
        self.set_pwm(PWMDuty(self.target_pwm))?;

        Ok(())
    }

    /// Resets internal controller state, using the new PWM as the current
    /// output value. This does not actually send the new PWM to the fans;
    /// that will occur on the next call to [run_control]
    pub fn reset(&mut self, initial_pwm: PWMDuty) -> Result<(), ThermalError> {
        if initial_pwm.0 > 100 {
            return Err(ThermalError::InvalidPWM);
        }
        self.target_pwm = initial_pwm.0;
        Ok(())
    }

    /// Attempts to set the PWM duty cycle of every fan in this group.
    ///
    /// Returns the last error if one occurred, but does not short circuit
    /// (i.e. attempts to set *all* fan duty cycles, even if one fails)
    pub fn set_pwm(&self, pwm: PWMDuty) -> Result<(), ThermalError> {
        if pwm.0 > 100 {
            return Err(ThermalError::InvalidPWM);
        }
        let mut last_err = Ok(());
        for (index, _sensor_id) in self.bsp.fans().iter().enumerate() {
            if let Err(e) = self.bsp.fan_control(Fan::from(index)).set_pwm(pwm)
            {
                last_err = Err(e);
            }
        }
        last_err.map_err(|_| ThermalError::DeviceError)
    }

    /// Sets the PWM for a single fan
    pub fn set_fan_pwm(
        &self,
        fan: Fan,
        pwm: PWMDuty,
    ) -> Result<(), ResponseCode> {
        self.bsp.fan_control(fan).set_pwm(pwm)
    }

    pub fn fan(&self, index: u8) -> Option<Fan> {
        let f = self.bsp.fans();

        if (index as usize) < f.len() {
            Some(Fan(index))
        } else {
            None
        }
    }

    pub fn set_watchdog(&self, wd: I2cWatchdog) -> Result<(), ResponseCode> {
        let mut result = Ok(());

        self.bsp.for_each_fctrl(|fctrl| {
            if let Err(e) = fctrl.set_watchdog(wd) {
                result = Err(e);
            }
        });

        result
    }
}
