// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    bsp::{self, Bsp, PowerBitmask},
    Fan, ThermalError, Trace,
};
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::{
    max31790::{I2cWatchdog, Max31790},
    nvme_bmc::NvmeBmc,
    sbtsi::Sbtsi,
    tmp117::Tmp117,
    tmp451::Tmp451,
    tse2004av::Tse2004Av,
    TempSensor,
};

use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_sensor_api::{Reading, Sensor as SensorApi, SensorError, SensorId};
use task_thermal_api::{ThermalAutoState, ThermalProperties};
use userlib::{
    sys_get_timer,
    units::{Celsius, PWMDuty, Rpm},
    TaskId,
};

////////////////////////////////////////////////////////////////////////////////

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
}

/// Represents a sensor in the system.
///
/// The sensor includes a device type, used to decide how to read it;
/// a free function that returns the raw `I2cDevice`, so that this can be
/// `const`); and the sensor ID, to post data to the `sensors` task.
pub struct TemperatureSensor {
    device: Device,
    builder: fn(TaskId) -> drv_i2c_api::I2cDevice,
    sensor_id: SensorId,
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
    fn read_temp(&self, i2c_task: TaskId) -> Result<Celsius, SensorReadError> {
        let dev = (self.builder)(i2c_task);
        let t = match &self.device {
            Device::Tmp117 => Tmp117::new(&dev).read_temperature()?,
            Device::CPU => Sbtsi::new(&dev).read_temperature()?,
            Device::Tmp451(t) => Tmp451::new(&dev, *t).read_temperature()?,
            Device::Dimm => Tse2004Av::new(&dev).read_temperature()?,
            Device::U2 | Device::M2 => NvmeBmc::new(&dev).read_temperature()?,
        };
        Ok(t)
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Combined error type for all of our temperature sensors
///
/// Most of them will only return an I2C `ResponseCode`, but in some cases,
/// they can report an error through in-band signalling (looking at you, NVMe)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SensorReadError {
    I2cError(ResponseCode),

    /// The sensor reported that data is either not present or too old
    NoData,

    /// The sensor reported a failure
    SensorFailure,

    /// The returned value is listed as reserved in the datasheet and does not
    /// represent a temperature.
    ReservedValue,

    /// The reply is structurally incorrect (wrong length, bad checksum, etc)
    CorruptReply,
}

impl From<drv_i2c_devices::tmp117::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::tmp117::Error) -> Self {
        use drv_i2c_devices::tmp117::Error::*;
        match s {
            BadRegisterRead { code, .. } => Self::I2cError(code),
        }
    }
}

impl From<drv_i2c_devices::tmp451::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::tmp451::Error) -> Self {
        use drv_i2c_devices::tmp451::Error::*;
        match s {
            BadRegisterRead { code, .. } => Self::I2cError(code),
            BadRegisterWrite { .. } => panic!(),
        }
    }
}

impl From<drv_i2c_devices::sbtsi::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::sbtsi::Error) -> Self {
        use drv_i2c_devices::sbtsi::Error::*;
        match s {
            BadRegisterRead { code, .. } => Self::I2cError(code),
        }
    }
}

impl From<drv_i2c_devices::tse2004av::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::tse2004av::Error) -> Self {
        use drv_i2c_devices::tse2004av::Error::*;
        match s {
            BadRegisterRead { code, .. } => Self::I2cError(code),
        }
    }
}

impl From<drv_i2c_devices::nvme_bmc::Error> for SensorReadError {
    fn from(s: drv_i2c_devices::nvme_bmc::Error) -> Self {
        use drv_i2c_devices::nvme_bmc::Error::*;
        match s {
            I2cError(v) => Self::I2cError(v),
            NoData => Self::NoData,
            SensorFailure => Self::SensorFailure,
            Reserved => Self::ReservedValue,
            InvalidLength | BadChecksum => Self::CorruptReply,
        }
    }
}

impl From<SensorReadError> for task_sensor_api::NoData {
    fn from(code: SensorReadError) -> task_sensor_api::NoData {
        match code {
            SensorReadError::I2cError(v) => v.into(),
            _ => Self::DeviceError,
        }
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
    model: ThermalProperties,

    /// Mask with bits set based on the Bsp's `power_mode` bits
    power_mode_mask: PowerBitmask,

    /// If we get `NoDevice` for a removable device, ignore it
    removable: bool,
}

impl InputChannel {
    pub const fn new(
        sensor: TemperatureSensor,
        model: ThermalProperties,
        power_mode_mask: PowerBitmask,
        removable: bool,
    ) -> Self {
        Self {
            sensor,
            model,
            power_mode_mask,
            removable,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// A `DynamicInputChannel` represents a temperature input channel with thermal
/// properties that are chosen at runtime, rather than baked into the BSP.
///
/// The _quantity_ of dynamic input channels is determined by the BSP, but their
/// thermal model and readings are passed into the `thermal` task over RPC
/// calls.
///
/// The motivating example is transceivers on the Sidecar switch; we know how
/// many of them could be present, but their thermal properties could vary
/// depending on what's plugged in.
#[derive(Clone, Copy)]
pub(crate) struct DynamicInputChannel {
    model: ThermalProperties,
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

    /// I2C task
    i2c_task: TaskId,

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
    power_mode: PowerBitmask,

    /// PID parameters, pulled from the BSP by default but user-modifiable
    pid_config: PidConfig,

    /// Dynamic inputs are fixed in number but configured at runtime.
    ///
    /// `None` values in this list are ignored.
    dynamic_inputs:
        [Option<DynamicInputChannel>; bsp::NUM_DYNAMIC_TEMPERATURE_INPUTS],
}

/// Represents the state of a temperature sensor, which either has a valid
/// reading or is marked as inactive (due to power state or being missing)
#[derive(Copy, Clone, Debug)]
enum TemperatureReading {
    /// Normal reading, timestamped using monotonic system time
    Valid(TimestampedTemperatureReading),

    /// This sensor is not used in the current power state
    Inactive,
}

/// Represents a temperature reading at the time at which it was taken
#[derive(Copy, Clone, Debug)]
struct TimestampedTemperatureReading {
    time_ms: u64,
    value: Celsius,
}

impl TimestampedTemperatureReading {
    /// Returns the worst-case temperature, given a current time and thermal
    /// model for this part.
    ///
    /// This only matters when samples are dropped or if there is significant
    /// lag in the sensors system; if we received a reading on this control
    /// cycle, then time_ms ≈ now_ms, so this is close to v.value (i.e. the most
    /// recent reading).
    ///
    /// Typically, time_ms is earlier (less) than now_ms, so this subtraction is
    /// safe.  If there's invalid data in the sensors task (i.e. readings
    /// claiming to be from the future), then this will saturate instead of
    /// underflowing.
    fn worst_case(&self, now_ms: u64, model: &ThermalProperties) -> Celsius {
        Celsius(
            self.value.0
                + now_ms.saturating_sub(self.time_ms) as f32 / 1000.0
                    * model.temperature_slew_deg_per_sec,
        )
    }
}

/// Configuration for a PID controller
#[derive(Copy, Clone)]
pub struct PidConfig {
    pub zero: f32,
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

        // To prevent integral windup, integral term needs to be clamped to values
        // can effect the output.
        let out_pd = cfg.zero + p_contribution + d_contribution;
        let (integral_min, integral_max) = if out_pd > output_limit {
            (-out_pd, 0.0)
        } else if out_pd < 0.0 {
            (0.0, -out_pd + output_limit)
        } else {
            (-out_pd, output_limit - out_pd)
        };
        self.integral = self.integral.clamp(integral_min, integral_max);

        // Clamp output values to valid range.
        let out = out_pd + self.integral;
        out.clamp(0.0, output_limit)
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

const TEMPERATURE_ARRAY_SIZE: usize =
    bsp::NUM_TEMPERATURE_INPUTS + bsp::NUM_DYNAMIC_TEMPERATURE_INPUTS;

/// This corresponds to states shown in RFD 276
///
/// All of our temperature arrays contain, in order
/// - I2C temperature inputs (read by this task)
/// - Dynamic temperature inputs (read by another task and passed in)
///
/// Note that the canonical temperatures are stored in the `sensors` task; we
/// copy them into these arrays for local operations.
enum ThermalControlState {
    /// Wait for each sensor to report in at least once
    ///
    /// (dynamic sensors must report in *if* they are present, i.e. not `None`
    /// in the `dynamic_inputs` array)
    Boot {
        values: [Option<TemperatureReading>; TEMPERATURE_ARRAY_SIZE],
    },

    /// Normal happy control loop
    Running {
        values: [TemperatureReading; TEMPERATURE_ARRAY_SIZE],
        pid: OneSidedPidState,
    },

    /// In the overheated state, one or more components has entered their
    /// critical temperature ranges.  We turn on fans at high power and record
    /// the time at which we entered this state; at a certain point, we will
    /// timeout and drop into `Uncontrolled` if components do not recover.
    Overheated {
        values: [TemperatureReading; TEMPERATURE_ARRAY_SIZE],
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
    fn write_temperature(&mut self, index: usize, reading: Reading) {
        let r = TemperatureReading::Valid(TimestampedTemperatureReading {
            time_ms: reading.timestamp,
            value: Celsius(reading.value),
        });
        match self {
            ThermalControlState::Boot { values } => {
                values[index] = Some(r);
            }
            ThermalControlState::Running { values, .. }
            | ThermalControlState::Overheated { values, .. } => {
                values[index] = r;
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
    pub fn new(bsp: &'a Bsp, i2c_task: TaskId, sensor_api: SensorApi) -> Self {
        Self {
            bsp,
            i2c_task,
            sensor_api,
            target_margin: Celsius(0.0f32),
            state: ThermalControlState::Boot {
                values: [None; TEMPERATURE_ARRAY_SIZE],
            },
            pid_config: bsp.pid_config,

            overheat_hysteresis: Celsius(1.0),
            overheat_timeout_ms: 60_000,

            power_mode: PowerBitmask::empty(), // no sensors active

            dynamic_inputs: [None; bsp::NUM_DYNAMIC_TEMPERATURE_INPUTS],
        }
    }

    pub fn set_pid(
        &mut self,
        z: f32,
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

        self.pid_config.zero = z;
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
            values: [None; TEMPERATURE_ARRAY_SIZE],
        };
        ringbuf_entry!(Trace::AutoState(self.get_state()));
    }

    /// Reads all temperature and fan RPM sensors, posting their results
    /// to the sensors task API.
    ///
    /// Records failed sensor reads and failed posts to the sensors task in
    /// the local ringbuf.
    pub fn read_sensors(&self) {
        // Read fan data and log it to the sensors task
        for (index, sensor_id) in self.bsp.fans.iter().enumerate() {
            let post_result =
                match self.bsp.fan_control(Fan::from(index)).fan_rpm() {
                    Ok(reading) => {
                        self.sensor_api.post_now(*sensor_id, reading.0.into())
                    }
                    Err(e) => {
                        ringbuf_entry!(Trace::FanReadFailed(*sensor_id, e));
                        self.sensor_api.nodata_now(*sensor_id, e.into())
                    }
                };
            if let Err(e) = post_result {
                ringbuf_entry!(Trace::PostFailed(*sensor_id, e));
            }
        }

        // Read miscellaneous temperature data and log it to the sensors task
        for s in self.bsp.misc_sensors.iter() {
            let post_result = match s.read_temp(self.i2c_task) {
                Ok(v) => self.sensor_api.post_now(s.sensor_id, v.0),
                Err(e) => {
                    ringbuf_entry!(Trace::MiscReadFailed(s.sensor_id, e));
                    self.sensor_api.nodata_now(s.sensor_id, e.into())
                }
            };
            if let Err(e) = post_result {
                ringbuf_entry!(Trace::PostFailed(s.sensor_id, e));
            }
        }

        // We read the power mode right before reading sensors, to avoid
        // potential TOCTOU issues; some sensors cannot be read if they are not
        // powered.
        let power_mode = self.bsp.power_mode();
        for s in self.bsp.inputs.iter() {
            let post_result = if power_mode.intersects(s.power_mode_mask) {
                match s.sensor.read_temp(self.i2c_task) {
                    Ok(v) => self.sensor_api.post_now(s.sensor.sensor_id, v.0),
                    Err(e) => {
                        // Record an error errors if the sensor is not removable
                        // or we get a unexpected error from a removable sensor
                        if !(s.removable
                            && e == SensorReadError::I2cError(
                                ResponseCode::NoDevice,
                            ))
                        {
                            ringbuf_entry!(Trace::SensorReadFailed(
                                s.sensor.sensor_id,
                                e
                            ));
                        }
                        self.sensor_api.nodata_now(s.sensor.sensor_id, e.into())
                    }
                }
            } else {
                // If the device isn't supposed to be on in the current power
                // state, then record it as Off in the sensors task.
                self.sensor_api.nodata_now(
                    s.sensor.sensor_id,
                    task_sensor_api::NoData::DeviceOff,
                )
            };
            if let Err(e) = post_result {
                ringbuf_entry!(Trace::PostFailed(s.sensor.sensor_id, e));
            }
        }

        // Note that this function does not send data about dynamic temperature
        // inputs to the `sensors` task!  This is because we don't know what
        // they are, so someone else has to do that.
    }

    /// Returns an iterator over tuples of `(value, thermal model)`
    ///
    /// The `values` array must contain `static_inputs.len()` +
    /// `dynamic_inputs.len()` values, in that order; this function will panic
    /// otherwise.
    ///
    /// In cases where dynamic inputs are not present (i.e. they are `None` in
    /// the array), the iterator will skip that entire tuple.
    fn zip_temperatures<'b, T>(
        values: &'b [T],
        (static_inputs, dynamic_inputs): (
            &'b [InputChannel],
            &'b [Option<DynamicInputChannel>],
        ),
    ) -> impl Iterator<Item = (&'b T, ThermalProperties)> {
        assert_eq!(values.len(), static_inputs.len() + dynamic_inputs.len());
        values
            .iter()
            .zip(
                static_inputs
                    .iter()
                    .map(|i| Some(i.model))
                    .chain(dynamic_inputs.iter().map(|i| i.map(|i| i.model))),
            )
            .filter_map(|(v, model)| model.map(|t| (v, t)))
    }

    /// An extremely simple thermal control loop.
    ///
    /// Returns an error if the control loop failed to read critical sensors;
    /// the caller should set us to some kind of fail-safe mode if this
    /// occurs.
    pub fn run_control(&mut self) -> Result<(), ThermalError> {
        let now_ms = sys_get_timer().now;

        // When the power mode changes, we may require a new set of sensors to
        // be online.  Reset the control state, waiting for all newly-required
        // sensors to come online before re-entering the control loop.
        let prev_power_mode = self.power_mode;
        self.power_mode = self.bsp.power_mode();
        if prev_power_mode != self.power_mode {
            ringbuf_entry!(Trace::PowerModeChanged(self.power_mode));
            self.reset_state();
        }

        // Load sensor readings from the `sensors` API.
        //
        // If the most recent reading is an error, then leave the previous value
        // in `self.state`.  When we're in the `Boot` state, this will leave the
        // value as `None`; when we're `Running`, it will maintain the previous
        // state, estimating a new temperature with the thermal model.
        for (i, s) in self.bsp.inputs.iter().enumerate() {
            if self.power_mode.intersects(s.power_mode_mask) {
                let sensor_id = s.sensor.sensor_id;
                let r = self.sensor_api.get_reading(sensor_id);
                match r {
                    Ok(r) => {
                        self.state.write_temperature(i, r);
                    }
                    Err(SensorError::NotPresent) if s.removable => {
                        // Ignore errors if the sensor is removable and the
                        // error indicates that it's not present.
                        self.state.write_temperature_inactive(i);
                    }
                    Err(_) => (),
                }
            } else {
                self.state.write_temperature_inactive(i);
            }
        }

        // The dynamic inputs don't depend on power mode; instead, they are
        // assumed to be present when a model exists in `self.dynamic_inputs`;
        // this model is set by external callers using
        // `update_dynamic_input` and `remove_dynamic_input`.
        for (i, sensor_id) in self.bsp.dynamic_inputs.iter().enumerate() {
            let index = i + self.bsp.inputs.len();
            match self.dynamic_inputs[i] {
                Some(..) => {
                    if let Ok(r) = self.sensor_api.get_reading(*sensor_id) {
                        self.state.write_temperature(index, r);
                    }
                }
                None => self.state.write_temperature_inactive(index),
            }
        }

        // A bit awkward, but we have to borrow these explicitly to work around
        // the lifetime checker, which won't let us call a &self function when
        // self.state is mutably borrowed.
        let inputs = (self.bsp.inputs, self.dynamic_inputs.as_slice());

        let control_result = match &mut self.state {
            ThermalControlState::Boot { values } => {
                let mut all_some = true;
                let mut any_power_down = false;
                let mut worst_margin = f32::MAX;
                for (v, model) in Self::zip_temperatures(values, inputs) {
                    match v {
                        Some(TemperatureReading::Valid(v)) => {
                            let temperature = v.worst_case(now_ms, &model);
                            any_power_down |=
                                model.should_power_down(temperature);
                            worst_margin =
                                worst_margin.min(model.margin(temperature).0);
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
                        values: values.map(Option::unwrap),
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
                for (v, model) in Self::zip_temperatures(values, inputs) {
                    if let TemperatureReading::Valid(v) = v {
                        let temperature = v.worst_case(now_ms, &model);
                        any_power_down |= model.should_power_down(temperature);
                        any_critical |= model.is_critical(temperature);

                        worst_margin =
                            worst_margin.min(model.margin(temperature).0);
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

                for (v, model) in Self::zip_temperatures(values, inputs) {
                    if let TemperatureReading::Valid(v) = v {
                        let temperature = v.worst_case(now_ms, &model);
                        all_subcritical &= model.is_sub_critical(
                            temperature,
                            self.overheat_hysteresis,
                        );
                        any_power_down |= model.should_power_down(temperature);
                        worst_margin =
                            worst_margin.min(model.margin(temperature).0);
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

    pub fn update_dynamic_input(
        &mut self,
        index: usize,
        model: ThermalProperties,
    ) -> Result<(), ThermalError> {
        if index >= bsp::NUM_DYNAMIC_TEMPERATURE_INPUTS {
            return Err(ThermalError::InvalidIndex);
        }
        // If we're adding a new dynamic input, then reset the state to `Boot`,
        // ensuring that we'll wait for that channel to provide us with at least
        // one valid reading before resuming the PID loop.
        if self.dynamic_inputs[index].is_none() {
            self.dynamic_inputs[index] = Some(DynamicInputChannel { model });
            self.reset_state();
        }
        Ok(())
    }

    pub fn remove_dynamic_input(
        &mut self,
        index: usize,
    ) -> Result<(), ThermalError> {
        if index >= bsp::NUM_DYNAMIC_TEMPERATURE_INPUTS {
            Err(ThermalError::InvalidIndex)
        } else {
            self.dynamic_inputs[index] = None;

            // Post this reading to the sensors task as well
            let sensor_id = self.bsp.dynamic_inputs[index];
            if let Err(e) = self.sensor_api.nodata_now(
                sensor_id,
                task_sensor_api::NoData::DeviceNotPresent,
            ) {
                ringbuf_entry!(Trace::PostFailed(sensor_id, e));
            }
            Ok(())
        }
    }
}
