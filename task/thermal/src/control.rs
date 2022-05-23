// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{bsp::BspT, Fan, TemperatureSensor, ThermalError, Trace};
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::max31790::I2cWatchdog;
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_sensor_api::Sensor as SensorApi;
use userlib::units::{Celsius, PWMDuty};

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
        let fctrl = self.bsp.fan_control();
        for (i, (fan, sensor_id)) in self.bsp.fans().iter().enumerate() {
            let post_result = match fctrl.fan_rpm(*fan) {
                Ok(reading) => {
                    self.sensor_api.post(*sensor_id, reading.0.into())
                }
                Err(e) => {
                    ringbuf_entry!(Trace::FanReadFailed(i, e));
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
        // Ignore errors and missing readings
        // TODO: handle this better
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
        let fctrl = self.bsp.fan_control();
        let mut last_err = Ok(());
        for (fan_id, _sensor_id) in self.bsp.fans() {
            if let Err(e) = fctrl.set_pwm(*fan_id, pwm) {
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
        self.bsp.fan_control().set_pwm(fan, pwm)
    }

    pub fn set_watchdog(&self, wd: I2cWatchdog) -> Result<(), ResponseCode> {
        self.bsp.fan_control().set_watchdog(wd)
    }
}
