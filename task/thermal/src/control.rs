// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{bsp::BspT, Fan, FanControl, TemperatureSensor, ThermalError};
use drv_i2c_api::ResponseCode;
use task_sensor_api::{Sensor as SensorApi, SensorId};
use userlib::units::{Celsius, PWMDuty};

////////////////////////////////////////////////////////////////////////////////

pub(crate) struct InputChannel {
    /// Temperature sensor
    sensor: TemperatureSensor,

    /// Maximum temperature for this part
    max_temp: Celsius,
}

impl InputChannel {
    pub fn new(sensor: TemperatureSensor, max_temp: Celsius) -> Self {
        Self { sensor, max_temp }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub(crate) struct OutputFans<'a> {
    /// Handle to the specific fan control IC
    fctrl: FanControl,

    /// List of fans (by fan id) and their matching RPM sensor ID.
    fans: &'a [(Fan, SensorId)],
}

impl<'a> OutputFans<'a> {
    pub fn new(fctrl: FanControl, fans: &'a [(Fan, SensorId)]) -> Self {
        Self { fctrl, fans }
    }

    /// Attempts to set the PWM duty cycle of every fan in this group.
    ///
    /// Returns the last error if one occurred, but does not short circuit
    /// (i.e. attempts to set *all* fan duty cycles, even if one fails)
    pub fn set_pwm(&self, pwm: PWMDuty) -> Result<(), ThermalError> {
        if pwm.0 > 100 {
            return Err(ThermalError::InvalidPWM);
        }
        let mut last_err = None;
        for (fan_id, _sensor_id) in self.fans {
            if let Err(e) = self.set_fan_pwm(*fan_id, pwm) {
                last_err = Some(e);
            }
        }
        if last_err.is_some() {
            Err(ThermalError::DeviceError)
        } else {
            Ok(())
        }
    }

    /// Sets the PWM for a single fan
    pub fn set_fan_pwm(
        &self,
        fan: Fan,
        pwm: PWMDuty,
    ) -> Result<(), ResponseCode> {
        self.fctrl.set_pwm(fan, pwm)
    }

    /// Reads fan RPM values, posting them to the sensor task API
    fn read_fans(&self, sensor_api: &SensorApi) {
        for (fan, sensor_id) in self.fans {
            match self.fctrl.fan_rpm(*fan) {
                Ok(reading) => {
                    sensor_api.post(*sensor_id, reading.0.into()).unwrap();
                }
                Err(e) => sensor_api.nodata(*sensor_id, e.into()).unwrap(),
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// The thermal control loop.
///
/// This object stores slices of sensors and fans, which must be owned
/// elsewhere; the standard pattern is to create static arrays in a
/// `struct Bsp` which is conditionally included based on board name.
pub(crate) struct ThermalControl<'a> {
    /// Sensors which are monitored as part of the control loop
    pub inputs: &'a [InputChannel],

    /// Fan output group.  Each `ThermalControl` is limited to a single
    /// fan control IC, but can choose which fans to control.
    pub outputs: OutputFans<'a>,

    /// Miscellaneous sensors, which are logged into the `sensor` task but
    /// do not affect the control loop
    pub misc_sensors: &'a [TemperatureSensor],

    /// Task to which we should post sensor data updates
    pub sensor_api: SensorApi,

    /// Dead band between increasing and decreasing fan speed
    pub hysteresis: Celsius,

    /// Target temperature margin. This must be >= 0; as it increases, parts
    /// are kept cooler than their max temperature ratings.
    pub target_margin: Celsius,

    /// Commanded PWM value (0-100) for every output channel
    pub target_pwm: u8,

    pub read_failed_count: u32,
    pub post_failed_count: u32,
}

impl<'a> ThermalControl<'a> {
    /// Constructs a new `ThermalControl` based on a `struct Bsp`. This
    /// requires that every BSP has the same internal structure,
    pub fn new<B: BspT>(bsp: &'a mut B, sensor_api: SensorApi) -> Self {
        let data = bsp.data();
        Self {
            inputs: data.inputs,
            outputs: OutputFans::new(data.fctrl, data.fans),
            misc_sensors: data.misc_sensors,
            sensor_api,
            hysteresis: Celsius(2.0f32),
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
    pub fn read_sensors(&mut self) -> Result<f32, ResponseCode> {
        self.outputs.read_fans(&self.sensor_api);

        for s in self.misc_sensors {
            let post_result = match s.read_temp() {
                Ok(v) => self.sensor_api.post(s.id, v.0),
                Err(e) => {
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
        for s in self.inputs {
            let post_result = match s.sensor.read_temp() {
                Ok(v) => {
                    let margin = s.max_temp.0 - v.0;
                    worst_margin = Some(match worst_margin {
                        Some(m) => margin.min(m),
                        None => margin,
                    });
                    self.sensor_api.post(s.sensor.id, v.0)
                }
                Err(e) => {
                    last_err = Err(e);
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

        // `worst_margin` is assigned whenever `last_err` is unassigned,
        // so this should not crash.  The exception is if there are
        // no sensors in `self.inputs`, but that's a pathological case,
        // so it's fine to crash.
        Ok(worst_margin.unwrap())
    }

    /// An extremely simple thermal control loop.
    ///
    /// Returns an error if the control loop failed to read critical sensors;
    /// the caller should set us to some kind of fail-safe mode if this
    /// occurs.
    pub fn run_control(&mut self) -> Result<(), ThermalError> {
        let worst_margin =
            self.read_sensors().map_err(|_| ThermalError::DeviceError)?;

        // Calculate the desired RPM change based on worst-case sensor
        if worst_margin >= self.target_margin.0 + self.hysteresis.0 {
            self.target_pwm = self.target_pwm.saturating_sub(1);
        } else if worst_margin < self.target_margin.0 {
            self.target_pwm = (self.target_pwm + 10).min(100);
        } else {
            // Don't modify PWM
        }

        // Send the new RPM to all of our fans
        self.outputs.set_pwm(PWMDuty(self.target_pwm))
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
}
