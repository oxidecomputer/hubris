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

    /// At the critical temperature, we should turn the fans up to 100% power in
    /// an attempt to cool the part.
    pub critical_temperature: Celsius,

    /// Temperature at which we drop into the A2 power state.  This should be
    /// below the part's nonrecoverable temperature.
    pub power_down_temperature: Celsius,

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

    /// How long to wait in the `Overheated` state before powering down
    overheat_timeout_ms: u64,

    /// Once we're in `Overheated`, how much does the temperature have to drop
    /// by before we return to `Normal`
    overheat_hysteresis: Celsius,

    /// Most recent power mode mask
    power_mode: u32,

    /// PID parameters, pulled from the BSP by default but user-modifiable
    pid_config: PidConfig,
}

/// Represents a temperature reading at the time at which it was taken
#[derive(Copy, Clone, Debug)]
enum TemperatureReading {
    /// Normal reading, timestamped using monotonic system time
    Valid { time_ms: u64, value: Celsius },

    /// This sensor is not used in the current power state
    Inactive,
}

/// Configuration for a PID controller
#[derive(Copy, Clone)]
pub struct PidConfig {
    pub gain_p: f32,
    pub gain_i: f32,
    pub gain_d: f32,
}

/// Represents a PID controller that can only push in one direction (i.e. the
/// output must always be positive).
struct OneSidedPidState {
    /// Previous (time, input) tuple, for derivative term
    prev_error: Option<f32>,

    /// Accumulated integral term, pre-multiplied by gain
    integral: f32,
}

impl OneSidedPidState {
    /// Attempts to drive the error to zero.
    ///
    /// The error and output are expected to have the same signs, i.e. a large
    /// positive error will produce a large positive output.
    fn run(&mut self, cfg: &PidConfig, error: f32, output_limit: f32) -> f32 {
        let p_contribution = cfg.gain_p * error;

        // Pre-multiply accumulated integral by gain, to make clamping easier
        // (this also means we can change the gain_i without glitches)
        self.integral += error * cfg.gain_i;

        // Calculate the derivative term if there was a previous error
        let d_contribution = if let Some(prev_error) = self.prev_error {
            (error - prev_error) * cfg.gain_d
        } else {
            0.0
        };
        self.prev_error = Some(error);

        // Calculate the P+D contribution separately, which is used to clamp the
        // integral term.
        let pd_contribution = p_contribution + d_contribution;
        let out = pd_contribution + self.integral;

        if out > output_limit {
            // Clamp the integral to the maximum value at which it can
            // contribute to the output.  For example:
            //
            // pd_contribution = 100 => clamp i to < 0
            // pd_contribution = 110 => clamp i to < 0
            // pd_contribution = 80  => clamp i to < 20
            let clamp = (output_limit - pd_contribution).max(0.0);
            self.integral = self.integral.min(clamp);

            output_limit
        } else if out < 0.0 {
            // Same idea as above, on the negative side.  For example:
            //
            // pd_contribution = 10  => clamp i to > -10
            // pd_contribution = -10 => clamp i to > 0
            let clamp = (-pd_contribution).min(0.0);
            self.integral = self.integral.max(clamp);

            0.0
        } else {
            out
        }
    }
}

impl Default for OneSidedPidState {
    fn default() -> Self {
        Self {
            prev_error: None,
            integral: 0.0,
        }
    }
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
        pid: OneSidedPidState,
    },

    /// In the overheated state, one or more components has entered their
    /// critical temperature ranges.  We turn on fans at high power and record
    /// the time at which we entered this state; at a certain point, we will
    /// timeout and drop into `Uncontrolled` if components do not recover.
    Overheated {
        values: [TemperatureReading; bsp::NUM_TEMPERATURE_INPUTS],
        start_time: u64,
    },

    /// The system cannot control the temperature; power down and wait for
    /// intervention from higher up the stack.
    Uncontrollable,
}

enum ControlResult {
    Pwm(PWMDuty),
    PowerDown,
}

impl ThermalControlState {
    fn write_temperature(
        &mut self,
        index: usize,
        time_ms: u64,
        value: Celsius,
    ) {
        match self {
            ThermalControlState::Boot { values } => {
                values[index] =
                    Some(TemperatureReading::Valid { time_ms, value })
            }
            ThermalControlState::Running { values, .. }
            | ThermalControlState::Overheated { values, .. } => {
                values[index] = TemperatureReading::Valid { time_ms, value }
            }
            ThermalControlState::Uncontrollable => (),
        }
    }

    fn write_temperature_inactive(&mut self, index: usize) {
        match self {
            ThermalControlState::Boot { values } => {
                values[index] = Some(TemperatureReading::Inactive)
            }
            ThermalControlState::Running { values, .. }
            | ThermalControlState::Overheated { values, .. } => {
                values[index] = TemperatureReading::Inactive;
            }
            ThermalControlState::Uncontrollable => (),
        }
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
            pid_config: bsp.pid_config,

            overheat_hysteresis: Celsius(1.0),
            overheat_timeout_ms: 60_000,

            power_mode: 0, // no sensors active
        }
    }

    pub fn set_pid(
        &mut self,
        p: f32,
        i: f32,
        d: f32,
    ) -> Result<(), ThermalError> {
        if p <= 0.0 || p.is_nan() || p.is_infinite() {
            return Err(ThermalError::InvalidParameter);
        }
        if i < 0.0 || i.is_nan() || i.is_infinite() {
            return Err(ThermalError::InvalidParameter);
        }
        if d < 0.0 || d.is_nan() || d.is_infinite() {
            return Err(ThermalError::InvalidParameter);
        }

        // If the incoming integral gain is zero, then it will never be able
        // to wind down the integral accumulator (which is pre-multiplied),
        // so clear it here.
        if let ThermalControlState::Running { pid, .. } = &mut self.state {
            if i == 0.0 {
                pid.integral = 0.0;
            }
        }

        self.pid_config.gain_p = p;
        self.pid_config.gain_i = i;
        self.pid_config.gain_d = d;

        Ok(())
    }

    pub fn set_margin(&mut self, margin: f32) -> Result<(), ThermalError> {
        if margin < 0.0 || margin.is_nan() || margin.is_infinite() {
            return Err(ThermalError::InvalidParameter);
        }
        self.target_margin = Celsius(margin);
        Ok(())
    }

    pub fn get_margin(&mut self) -> f32 {
        self.target_margin.0
    }

    /// Resets the control state and the PID configuration
    pub fn reset(&mut self) {
        self.reset_state();

        // Reset the PID configuration from the BSP
        self.pid_config = self.bsp.pid_config;

        // Set the target_margin to 0, indicating no overcooling
        self.target_margin = Celsius(0.0f32);
    }

    /// Resets the control state
    fn reset_state(&mut self) {
        self.state = ThermalControlState::Boot {
            values: [None; bsp::NUM_TEMPERATURE_INPUTS],
        };
        ringbuf_entry!(Trace::AutoState(self.get_state()));
    }

    /// Reads all temperature and fan RPM sensors, posting their results
    /// to the sensors task API and recording them in `self.state`.
    ///
    /// Records failed sensor reads and failed posts to the sensors task in
    /// the local ringbuf.
    pub fn read_sensors(&mut self, now_ms: u64) {
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
            if let Err(e) = post_result {
                ringbuf_entry!(Trace::PostFailed(*sensor_id, e));
            }
        }

        // Read miscellaneous temperature data and log it to the sensors task
        for (i, s) in self.bsp.misc_sensors.iter().enumerate() {
            let post_result = match s.read_temp() {
                Ok(v) => self.sensor_api.post(s.id, v.0),
                Err(e) => {
                    ringbuf_entry!(Trace::MiscReadFailed(i, e));
                    self.sensor_api.nodata(s.id, e.into())
                }
            };
            if let Err(e) = post_result {
                ringbuf_entry!(Trace::PostFailed(s.id, e));
            }
        }

        // When the power mode changes, we may require a new set of sensors to
        // be online.  Reset the control state, waiting for all newly-required
        // sensors to come online before re-entering the control loop.
        let prev_power_mode = self.power_mode;
        self.power_mode = self.bsp.power_mode();
        if prev_power_mode != self.power_mode {
            ringbuf_entry!(Trace::PowerModeChanged(self.power_mode));
            self.reset_state();
        }

        for (i, s) in self.bsp.inputs.iter().enumerate() {
            let post_result = match s.sensor.read_temp() {
                Ok(v) => {
                    if (s.power_mode_mask & self.power_mode) != 0 {
                        self.state.write_temperature(i, now_ms, v);
                    } else {
                        self.state.write_temperature_inactive(i);
                    }
                    self.sensor_api.post(s.sensor.id, v.0)
                }
                Err(e) => {
                    // Ignore errors if
                    // a) this sensor shouldn't be on in this power mode, or
                    // b) the sensor is removable and not present
                    if (s.power_mode_mask & self.power_mode) == 0
                        || (s.removable && e == ResponseCode::NoDevice)
                    {
                        self.state.write_temperature_inactive(i);
                    } else {
                        // By not calling self.state.write_temperature_*, we're
                        // leaving the stale data into the controller; if the
                        // sensor failure is persistent, then thermal loop will
                        // eventually handle it (once the modelled worst-case
                        // temperature is sufficiently high)
                        ringbuf_entry!(Trace::SensorReadFailed(i, e));
                    }
                    self.sensor_api.nodata(s.sensor.id, e.into())
                }
            };
            if let Err(e) = post_result {
                ringbuf_entry!(Trace::PostFailed(s.sensor.id, e));
            }
        }
    }

    /// An extremely simple thermal control loop.
    ///
    /// Returns an error if the control loop failed to read critical sensors;
    /// the caller should set us to some kind of fail-safe mode if this
    /// occurs.
    pub fn run_control(&mut self, now_ms: u64) -> Result<(), ThermalError> {
        self.read_sensors(now_ms);

        let control_result = match &mut self.state {
            ThermalControlState::Boot { values } => {
                let mut all_some = true;
                let mut any_power_down = false;
                let mut worst_margin = f32::MAX;
                for (v, i) in values.iter().zip(self.bsp.inputs.iter()) {
                    match v {
                        Some(TemperatureReading::Valid { value, time_ms }) => {
                            // Model the current temperature based on the last
                            // reading and the worst-case slew rate.  This only
                            // matters when samples are dropped; if we received
                            // a reading on this control cycle, then time_ms ==
                            // now_ms, so this becomes v.value (i.e. the most
                            // recent reading).
                            let temperature = value.0
                                + (time_ms - now_ms) as f32 / 1000.0
                                    * i.temps.temperature_slew_deg_per_sec;
                            any_power_down |=
                                temperature >= i.temps.power_down_temperature.0;
                            worst_margin = worst_margin.min(
                                i.temps.target_temperature.0 - temperature,
                            );
                        }
                        Some(TemperatureReading::Inactive) => {
                            // Inactive sensors are ignored, but do not gate us
                            // from transitioning to `Running`
                        }
                        None => all_some = false,
                    }
                }
                if any_power_down {
                    self.state = ThermalControlState::Uncontrollable;
                    ringbuf_entry!(Trace::AutoState(self.get_state()));

                    ControlResult::PowerDown
                } else if all_some {
                    // Transition to the Running state and run a single
                    // iteration of the PID control loop.
                    let mut pid = OneSidedPidState::default();
                    let pwm = pid.run(
                        &self.pid_config,
                        self.target_margin.0 - worst_margin,
                        100.0,
                    );
                    self.state = ThermalControlState::Running {
                        values: values.map(|i| i.unwrap()),
                        pid,
                    };
                    ringbuf_entry!(Trace::AutoState(self.get_state()));

                    ControlResult::Pwm(PWMDuty(pwm as u8))
                } else {
                    ControlResult::Pwm(PWMDuty(100))
                }
            }
            ThermalControlState::Running { values, pid } => {
                let mut any_power_down = false;
                let mut any_critical = false;
                let mut worst_margin = f32::MAX;
                // Remember, positive margin means that all parts are happily
                // below their max temperature; negative means someone is
                // overheating.  We want to pick the _smallest_ margin, since
                // that's the part which is most overheated.
                for (v, i) in values.iter().zip(self.bsp.inputs.iter()) {
                    if let TemperatureReading::Valid { value, time_ms } = v {
                        let temperature = value.0
                            + (time_ms - now_ms) as f32 / 1000.0
                                * i.temps.temperature_slew_deg_per_sec;
                        any_power_down |=
                            temperature >= i.temps.power_down_temperature.0;
                        any_critical |=
                            temperature >= i.temps.critical_temperature.0;

                        worst_margin = worst_margin
                            .min(i.temps.target_temperature.0 - temperature);
                    }
                }

                if any_power_down {
                    self.state = ThermalControlState::Uncontrollable;
                    ringbuf_entry!(Trace::AutoState(self.get_state()));

                    ControlResult::PowerDown
                } else if any_critical {
                    self.state = ThermalControlState::Overheated {
                        values: *values,
                        start_time: now_ms,
                    };
                    ringbuf_entry!(Trace::AutoState(self.get_state()));

                    ControlResult::Pwm(PWMDuty(100))
                } else {
                    // We adjust the worst component margin by our target
                    // margin, which must be > 0.  This effectively tells the
                    // control loop to overcool the system.
                    //
                    // `PidControl::run` expects the sign of the input and
                    // output to match, so we negate things here: if the worst
                    // margin is negative (i.e. the system is overheating), then
                    // the input to `run` is positive, because we want a
                    // positive fan speed.
                    let pwm = pid.run(
                        &self.pid_config,
                        self.target_margin.0 - worst_margin,
                        100.0,
                    );
                    ControlResult::Pwm(PWMDuty(pwm as u8))
                }
            }
            ThermalControlState::Overheated { values, start_time } => {
                let mut all_subcritical = true;
                let mut any_power_down = false;
                let mut worst_margin = f32::MAX;

                for (v, i) in values.iter().zip(self.bsp.inputs.iter()) {
                    if let TemperatureReading::Valid { value, time_ms } = v {
                        let temperature = value.0
                            + (time_ms - now_ms) as f32 / 1000.0
                                * i.temps.temperature_slew_deg_per_sec;

                        all_subcritical &= temperature
                            < i.temps.critical_temperature.0
                                - self.overheat_hysteresis.0;
                        any_power_down |=
                            temperature >= i.temps.power_down_temperature.0;
                        worst_margin = worst_margin
                            .min(i.temps.target_temperature.0 - temperature);
                    }
                }

                if any_power_down {
                    self.state = ThermalControlState::Uncontrollable;
                    ringbuf_entry!(Trace::AutoState(self.get_state()));

                    ControlResult::PowerDown
                } else if all_subcritical {
                    // Transition to the Running state and run a single
                    // iteration of the PID control loop.
                    let mut pid = OneSidedPidState::default();
                    let pwm = pid.run(
                        &self.pid_config,
                        self.target_margin.0 - worst_margin,
                        100.0,
                    );
                    self.state = ThermalControlState::Running {
                        values: *values,
                        pid,
                    };
                    ringbuf_entry!(Trace::AutoState(self.get_state()));

                    ControlResult::Pwm(PWMDuty(pwm as u8))
                } else if now_ms > *start_time + self.overheat_timeout_ms {
                    // If blasting the fans hasn't cooled us down in this amount
                    // of time, then something is terribly wrong - abort!
                    self.state = ThermalControlState::Uncontrollable;
                    ringbuf_entry!(Trace::AutoState(self.get_state()));

                    ControlResult::PowerDown
                } else {
                    ControlResult::Pwm(PWMDuty(100))
                }
            }
            ThermalControlState::Uncontrollable => ControlResult::PowerDown,
        };

        match control_result {
            ControlResult::Pwm(target_pwm) => {
                // Send the new RPM to all of our fans
                ringbuf_entry!(Trace::ControlPwm(target_pwm.0));
                self.set_pwm(target_pwm)?;
            }
            ControlResult::PowerDown => {
                if let Err(e) = self.bsp.power_down() {
                    ringbuf_entry!(Trace::PowerDownFailed(e));
                }
                self.set_pwm(PWMDuty(0))?;
            }
        }

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
