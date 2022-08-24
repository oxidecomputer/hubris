// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    bsp::{self, Bsp},
    Fan, ThermalError, Trace,
};
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::max31790::{I2cWatchdog, Max31790};
use drv_i2c_devices::TempSensor;
use drv_i2c_devices::{
    sbtsi::Sbtsi, tmp117::Tmp117, tmp451::Tmp451, tse2004av::Tse2004Av,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_sensor_api::{Sensor as SensorApi, SensorId};
use task_thermal_api::ThermalAutoState;
use userlib::{
    sys_get_timer,
    units::{Celsius, PWMDuty, Rpm},
};

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

/// An `InputChannel` represents a temperature sensor associated with a
/// particular component in the system.
pub(crate) struct InputChannel {
    /// Temperature sensor
    sensor: TemperatureSensor,

    /// Thermal properties of the associated component
    temps: ThermalProperties,

    /// Mask with bits set based on the Bsp's `power_mode` bits
    power_mode_mask: u32,

    /// If we get `NoDevice` for a removable device, ignore it
    removable: bool,
}

/// Properties for a particular part in the system
pub(crate) struct ThermalProperties {
    /// Target temperature for this part
    pub target_temperature: Celsius,

    /// Temperature at which we should take dramatic action to cool the part
    pub critical_temperature: Celsius,

    /// Temperature at which the part may be damaged
    pub non_recoverable_temperature: Celsius,

    /// Maximum slew rate of temperature, measured in °C per second
    ///
    /// The slew rate is used to model worst-case temperature if we haven't
    /// heard from a chip in a while (e.g. due to dropped samples)
    pub temperature_slew_deg_per_sec: f32,
}

impl InputChannel {
    pub fn new(
        sensor: TemperatureSensor,
        temps: ThermalProperties,
        power_mode_mask: u32,
        removable: bool,
    ) -> Self {
        Self {
            sensor,
            temps,
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
pub(crate) struct ThermalControl<'a> {
    /// Reference to board-specific parameters
    bsp: &'a Bsp,

    /// Task to which we should post sensor data updates
    sensor_api: SensorApi,

    /// Target temperature margin. This must be >= 0; as it increases, parts
    /// are kept cooler than their target temperature value.
    target_margin: Celsius,

    /// Controller state
    state: ThermalControlState,

    /// Number of sensor reads which failed
    read_failed_count: u32,

    /// Number of sensor posts which failed
    post_failed_count: u32,
}

/// Represents a temperature reading at the time at which it was taken
///
/// (using monotonic system time)
#[derive(Copy, Clone, Debug)]
struct TemperatureReading {
    time_ms: u64,
    value: Celsius,
}

/// Configuration for a PID controller
pub struct PidConfig {
    pub gain_p: f64,
    pub gain_i: f64,
    pub gain_d: f64,
}

struct PidState {
    /// Previous (time, input) tuple, for derivative term
    prev: Option<(u64, f64)>,

    /// Accumulated integral term
    integral: f64,
}

/// This corresponds to states shown in RFD 276
enum ThermalControlState {
    /// Wait for each sensor to report in at least once
    Boot {
        values: [Option<TemperatureReading>; bsp::NUM_TEMPERATURE_INPUTS],
    },

    /// Normal happy control loop
    Running {
        values: [TemperatureReading; bsp::NUM_TEMPERATURE_INPUTS],
        pid: PidState,
    },

    /// In the overheated state, one or more components has entered their
    /// critical temperature ranges.  We turn on fans and record the time at
    /// which we entered this state; at a certain point, we will timeout and
    /// drop into `Uncontrolled`.
    Overheated {
        values: [TemperatureReading; bsp::NUM_TEMPERATURE_INPUTS],
        start_time: u64,
    },

    /// The system cannot control the temperature; power down and wait for
    /// intervention from higher up the stack.
    Uncontrollable,
}

impl ThermalControlState {
    fn control(
        &mut self,
        target_margin: Celsius,
        inputs: &[InputChannel],
        pid_cfg: &PidConfig,
    ) -> u8 {
        unimplemented!()
    }
    fn write_temperature(&mut self, index: usize, time: u64, value: Celsius) {
        unimplemented!()
    }
}

impl<'a> ThermalControl<'a> {
    /// Constructs a new `ThermalControl` based on a `struct Bsp`. This
    /// requires that every BSP has the same internal structure,
    pub fn new(bsp: &'a Bsp, sensor_api: SensorApi) -> Self {
        Self {
            bsp,
            sensor_api,
            target_margin: Celsius(0.0f32),
            state: ThermalControlState::Boot {
                values: [None; bsp::NUM_TEMPERATURE_INPUTS],
            },
            read_failed_count: 0,
            post_failed_count: 0,
        }
    }

    /// Resets the control state
    pub fn reset(&mut self) {
        self.state = ThermalControlState::Boot {
            values: [None; bsp::NUM_TEMPERATURE_INPUTS],
        };
        self.target_margin = Celsius(0.0f32);
        self.read_failed_count = 0;
        self.post_failed_count = 0;
        // The fan speed will be applied on the next controller iteration
    }

    /// Reads all temperature and fan RPM sensors, posting their results
    /// to the sensors task API and recording them in `self.state`.
    ///
    /// Records failed sensor reads and failed posts to the sensors task in
    /// `self.read_failed_count` and `self.post_failed_count` respectively.
    pub fn read_sensors(&mut self) {
        // Read fan data and log it to the sensors task
        for (index, sensor_id) in self.bsp.fans.iter().enumerate() {
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
        for (i, s) in self.bsp.misc_sensors.iter().enumerate() {
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
        let power_mode = self.bsp.power_mode();
        let now = sys_get_timer().now;
        for (i, s) in self.bsp.inputs.iter().enumerate() {
            let post_result = match s.sensor.read_temp() {
                Ok(v) => {
                    if (s.power_mode_mask & power_mode) != 0 {
                        self.state.write_temperature(i, now, v);
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
                        ringbuf_entry!(Trace::SensorReadFailed(i, e));
                    }
                    self.sensor_api.nodata(s.sensor.id, e.into())
                }
            };
            if post_result.is_err() {
                self.post_failed_count = self.post_failed_count.wrapping_add(1);
            }
        }
    }

    /// An extremely simple thermal control loop.
    ///
    /// Returns an error if the control loop failed to read critical sensors;
    /// the caller should set us to some kind of fail-safe mode if this
    /// occurs.
    pub fn run_control(&mut self) -> Result<(), ThermalError> {
        self.read_sensors();

        let target_pwm = self.state.control(
            self.target_margin,
            &self.bsp.inputs,
            &self.bsp.pid_config,
        );

        // Send the new RPM to all of our fans
        ringbuf_entry!(Trace::ControlPwm(target_pwm));
        self.set_pwm(PWMDuty(target_pwm))?;

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
        for (index, _sensor_id) in self.bsp.fans.iter().enumerate() {
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
        let f = &self.bsp.fans;

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

    pub fn get_state(&self) -> ThermalAutoState {
        match self.state {
            ThermalControlState::Boot { .. } => ThermalAutoState::Boot,
            ThermalControlState::Running { .. } => ThermalAutoState::Running,
            ThermalControlState::Overheated { .. } => {
                ThermalAutoState::Overheated
            }
            ThermalControlState::Uncontrollable => {
                ThermalAutoState::Uncontrollable
            }
        }
    }
}
