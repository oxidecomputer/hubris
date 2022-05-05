// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Fan, FanControl, PWMDuty, TemperatureSensor};
use task_sensor_api::{Sensor as SensorApi, SensorId};
use userlib::units::Celsius;

pub(crate) struct InputChannel {
    /// Temperature sensor
    pub sensor: TemperatureSensor,

    /// Maximum temperature for this part
    pub max_temp: Celsius,
}

pub(crate) struct OutputFans<'a> {
    /// Handle to the specific fan control IC
    pub fctrl: &'a FanControl,

    /// List of fans (by fan id) and their matching RPM sensor ID.
    pub fans: &'a [(Fan, SensorId)],
}

impl<'a> OutputFans<'a> {
    fn set_pwm(&self, pwm: PWMDuty) {
        for (fan_id, _sensor_id) in self.fans {
            self.fctrl.set_pwm(*fan_id, pwm);
        }
    }
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

/// The thermal control loop.
///
/// This object stores slices of sensors and fans, which must be owned
/// elsewhere; the standard pattern is to create static arrays in a
/// `struct Bsp` which is conditionally included based on board name.
pub(crate) struct ThermalControl<'a> {
    /// Sensors which are monitored as part of the control loop
    pub inputs: &'a mut [InputChannel],

    /// Fan output group.  Each `ThermalControl` is limited to a single
    /// fan control IC, but can choose which fans to control.
    pub outputs: OutputFans<'a>,

    /// Miscellaneous sensors, which are logged into the `sensor` task but
    /// do not affect the control loop
    pub misc_sensors: &'a mut [TemperatureSensor],

    /// Task to which we should post sensor data updates
    pub sensor_api: SensorApi,

    /// Dead band between increasing and decreasing fan speed
    pub hysteresis: Celsius,

    /// Target temperature margin. This must be >= 0; as it increases, parts
    /// are kept cooler than their max temperature ratings.
    pub target_margin: Celsius,

    /// Commanded PWM value (0-100) for every output channel
    pub target_pwm: u8,
}

enum Command {
    IncreaseRpm,
    DecreaseRpm,
    NoChange,
}

impl<'a> ThermalControl<'a> {
    /// An extremely simple thermal control loop.
    pub fn step(&mut self) {
        self.outputs.read_fans(&self.sensor_api);

        // Margin represents the difference between max and current temperature
        // for the worst-case part.  Positive margin means that all parts are
        // below their max temperature; negative means someone is overheating.
        let mut worst_margin = None;
        let mut last_err = None;
        for s in self.inputs.iter_mut() {
            match s.sensor.read_temp() {
                Ok(v) => {
                    self.sensor_api.post(s.sensor.id, v.0).unwrap();
                    let margin = s.max_temp.0 - v.0;
                    worst_margin = Some(match worst_margin {
                        Some(m) => margin.min(m),
                        None => margin,
                    });
                }
                Err(e) => {
                    self.sensor_api.nodata(s.sensor.id, e.into()).unwrap();
                    last_err = Some(e);
                }
            }
        }

        // Calculate the desired RPM change based on worst-case sensor
        let cmd = if let Some(m) = worst_margin {
            if m >= self.target_margin.0 + self.hysteresis.0 {
                Command::DecreaseRpm
            } else if m < self.target_margin.0 {
                Command::IncreaseRpm
            } else {
                Command::NoChange
            }
        } else {
            Command::IncreaseRpm
        };

        // Apply RPM change
        match cmd {
            Command::IncreaseRpm => {
                self.target_pwm += 10;
            }
            Command::DecreaseRpm => {
                self.target_pwm = self.target_pwm.saturating_sub(1);
            }
            Command::NoChange => (),
        }
        self.target_pwm = self.target_pwm.clamp(0, 100);

        // Send the new RPM to all of our fans
        self.outputs.set_pwm(PWMDuty(self.target_pwm));
    }
}
