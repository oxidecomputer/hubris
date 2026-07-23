// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    ThermalError, Trace,
    bsp::{Bsp, PowerBitmask},
};
use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_i2c_devices::{
    TempSensor,
    emc2305::Emc2305,
    max31790::{I2cWatchdog, Max31790},
    nvme_bmc::NvmeBmc,
    pct2075::Pct2075,
    sbtsi::Sbtsi,
    tmp117::Tmp117,
    tmp451::Tmp451,
    tse2004av::Tse2004Av,
};

use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_sensor_api::{Sensor as SensorApi, SensorId};
use task_thermal_api::{SensorReadError, ThermalAutoState, ThermalProperties};
use userlib::{
    TaskId, sys_get_timer,
    units::{Celsius, PWMDuty, Rpm},
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
    LM75,
}

/// Represents a sensor in the system.
///
/// The sensor includes a device type, used to decide how to read it;
/// a free function that returns the raw `I2cDevice`, so that this can be
/// `const`); and the sensor ID, to post data to the `sensors` task.
#[allow(dead_code)] // not all BSPS
pub struct TemperatureSensor {
    device: Device,
    builder: fn(TaskId) -> drv_i2c_api::I2cDevice,
    pub sensor_id: SensorId,
}

impl TemperatureSensor {
    #[allow(dead_code)] // not all BSPS
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

////////////////////////////////////////////////////////////////////////////////

/// Enum representing any of our fan controller types, bound to one of their
/// fans.  This lets us handle heterogeneous fan controller ICs generically
/// (although there's only one at the moment)
#[allow(dead_code)] // a typical BSP uses only _one_ of these
pub enum FanControl<'a> {
    Max31790(&'a Max31790, drv_i2c_devices::max31790::Fan),
    Emc2305(&'a Emc2305, drv_i2c_devices::emc2305::Fan),
}

impl<'a> FanControl<'a> {
    pub fn set_pwm(&self, pwm: PWMDuty) -> Result<(), ResponseCode> {
        match self {
            Self::Max31790(m, fan) => m.set_pwm(*fan, pwm),
            Self::Emc2305(m, fan) => m.set_pwm(*fan, pwm),
        }
    }

    pub fn fan_rpm(&self) -> Result<Rpm, ResponseCode> {
        match self {
            Self::Max31790(m, fan) => m.fan_rpm(*fan),
            Self::Emc2305(m, fan) => m.fan_rpm(*fan),
        }
    }

    pub fn set_watchdog(&self, wd: I2cWatchdog) -> Result<(), ResponseCode> {
        match self {
            Self::Max31790(m, _fan) => m.set_watchdog(wd),
            Self::Emc2305(m, _fan) => {
                // The EMC2305 doesn't support setting the watchdog time, just
                // whether it's enabled or disabled
                m.set_watchdog(!matches!(wd, I2cWatchdog::Disabled))
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// An `InputChannel` represents a temperature sensor associated with a
/// particular component in the system.
pub(crate) struct InputChannel {
    /// Temperature sensor
    pub sensor: TemperatureSensor,

    /// Thermal properties of the associated component
    pub model: ThermalProperties,

    /// Mask with bits set based on the Bsp's `power_mode` bits
    pub power_mode_mask: PowerBitmask,

    /// Channel type
    pub ty: ChannelType,

    pub last_reading: Option<TemperatureReading>,
}

#[derive(Copy, Clone, Eq, PartialEq)]
#[allow(dead_code)] // a typical BSP uses only a subset of these.
pub(crate) enum ChannelType {
    /// `MustBePresent` is exactly what it says on the tin
    ///
    /// If this sensor isn't present, the thermal loop will remain in the
    /// `Booting` state until it appears; if the sensor disappears during later
    /// operation, we will model its temperature based on the simple thermal
    /// model.
    MustBePresent,

    /// `Removable` means that this sensor may not be present, and that's okay
    ///
    /// Specifically, we can detect its non-presence by I2C NACKs, which are
    /// translated to `ResponseCode::NoDevice` by the I2C driver and then to
    /// `SensorError::NotPresent` in the sensors task.
    ///
    /// The absense of this sensor does not block exiting `Booting` state, and
    /// if the sensor is `NotPresent`, we ignore it for the purposes of the
    /// thermal loop.
    ///
    /// Note that other error codes are **not** ignored!  For example, if we got
    /// a `BusLocked` error code when trying to read the sensor, we would treat
    /// that as a missed reading but the sensor would remain present; the loop
    /// would then use the thermal model to estimate temperature based on the
    /// last known reading.
    Removable,

    /// The sensor may disappear for reasons other than `NotPresent`
    ///
    /// We are living with the unfortunate reality that our U.2 drives very
    /// occasionally lock up, pulling I2C low and refusing to talk any further
    /// (hardware-gimlet#1946). The issue appears to be drive-specific, e.g.
    /// moving a problematic drive to a different position moves the lockup.
    ///
    /// `RemovableAndErrorProne` means that we will treat _any_ error as the
    /// device being not present.
    RemovableAndErrorProne,
}

impl InputChannel {
    #[allow(dead_code)] // not all BSPS
    pub const fn new(
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
            last_reading: None,
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
#[allow(dead_code)] // Not all bsps have dynamic inputs
pub(crate) struct DynamicInputChannel {
    pub sensor_id: SensorId,
    pub model: ThermalProperties,
    pub last_reading: Option<TemperatureReading>,
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone)]
#[allow(dead_code)] // used only by the debugger
pub struct TimestampedSensorError {
    pub timestamp: u64,
    pub id: SensorId,
    pub err: SensorReadError,
}

#[derive(Copy, Clone)]
pub struct ThermalSensorErrors {
    pub values: [Option<TimestampedSensorError>; 16],
    pub next: u32,
}

impl ThermalSensorErrors {
    pub const fn new() -> Self {
        Self {
            values: [None; 16],
            next: 0,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn push(&mut self, id: SensorId, err: SensorReadError) {
        if let Some(v) = self.values.get_mut(self.next as usize) {
            let timestamp = userlib::sys_get_timer().now;
            *v = Some(TimestampedSensorError { id, err, timestamp });
            self.next += 1;
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Tracks whether a MAX31790 fan controller has been initialized, and
/// initializes it on demand when accessed, if necessary.
///
/// Because initializing the fan controller can fail due to a transient bus
/// error, we don't panic if an initial attempt to initialize it as soon as the
/// `thermal` task starts fails. Because the fan controller's I2C watchdog will
/// simply run the fans at 100% if we aren't able to talk to it right away, the
/// `thermal` task should keep running, publishing sensor measurements, and
/// periodically trying to reach the fan controller until we're able to
/// initialize it successfully. Thus, we wrap it in this struct to track whether
/// it's been successfully initialized yet.
pub(crate) struct Max31790State {
    max31790: Max31790,
    initialized: bool,
}

impl Max31790State {
    #[allow(dead_code)]
    pub(crate) fn new(dev: &I2cDevice) -> Self {
        let mut this = Self {
            max31790: Max31790::new(dev),
            initialized: false,
        };
        retry_init(|| this.initialize().map(|_| ()));
        this
    }

    /// Access the fan controller, attempting to initialize it if it has not yet
    /// been initialized.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn try_initialize(
        &mut self,
    ) -> Result<&mut Max31790, ControllerInitError> {
        if self.initialized {
            return Ok(&mut self.max31790);
        }

        self.initialize()
    }

    // Slow path that actually performs initialization. This is "outlined" so
    // that we can avoid pushing a stack frame in the case where we just need to
    // check a bool and return a pointer.
    #[inline(never)]
    fn initialize(&mut self) -> Result<&mut Max31790, ControllerInitError> {
        self.max31790.initialize().map_err(|e| {
            ringbuf_entry!(Trace::FanControllerInitError(e));
            ControllerInitError(e)
        })?;

        self.initialized = true;
        ringbuf_entry!(Trace::FanControllerInitialized);
        Ok(&mut self.max31790)
    }
}

/// Tracks whether a EMC2305 fan controller has been initialized, and
/// initializes it on demand when accessed, if necessary.
///
/// This is copy-pasted from [`Max31790`]
pub(crate) struct Emc2305State {
    emc2305: Emc2305,
    fan_count: u8,
    initialized: bool,
}

impl Emc2305State {
    #[allow(dead_code)]
    pub(crate) fn new(dev: &I2cDevice, fan_count: u8) -> Self {
        let mut this = Self {
            emc2305: Emc2305::new(dev),
            fan_count,
            initialized: false,
        };
        retry_init(|| this.initialize().map(|_| ()));
        this
    }

    /// Access the fan controller, attempting to initialize it if it has not yet
    /// been initialized.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn try_initialize(
        &mut self,
    ) -> Result<&mut Emc2305, ControllerInitError> {
        if self.initialized {
            return Ok(&mut self.emc2305);
        }

        self.initialize()
    }

    // Slow path that actually performs initialization. This is "outlined" so
    // that we can avoid pushing a stack frame in the case where we just need to
    // check a bool and return a pointer.
    #[inline(never)]
    fn initialize(&mut self) -> Result<&mut Emc2305, ControllerInitError> {
        self.emc2305.initialize(self.fan_count).map_err(|e| {
            ringbuf_entry!(Trace::FanControllerInitError(e));
            ControllerInitError(e)
        })?;

        self.initialized = true;
        ringbuf_entry!(Trace::FanControllerInitialized);
        Ok(&mut self.emc2305)
    }
}

/// Helper function to retry initialization several times, logging errors
fn retry_init<F: FnMut() -> Result<(), ControllerInitError>>(mut init: F) {
    // When we first start up, try to initialize the fan controller a few
    // times, in case there's a transient I2C error.
    for remaining in (0..3).rev() {
        if init().is_ok() {
            break;
        }
        ringbuf_entry!(Trace::FanControllerInitRetry { remaining });
    }
}

pub(crate) struct ControllerInitError(ResponseCode);

impl From<ControllerInitError> for ThermalError {
    fn from(_: ControllerInitError) -> Self {
        ThermalError::FanControllerUninitialized
    }
}

impl From<ControllerInitError> for SensorReadError {
    fn from(ControllerInitError(code): ControllerInitError) -> Self {
        SensorReadError::I2cError(code)
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
    bsp: &'a mut Bsp,

    /// I2C task
    i2c_task: TaskId,

    /// Task to which we should post sensor data updates
    sensor_api: SensorApi,

    /// Target temperature margin. This must be >= 0; as it increases, parts
    /// are kept cooler than their target temperature value.
    target_margin: Celsius,

    /// Controller state
    state: ThermalControlState,

    /// Most recent power mode mask
    power_mode: PowerBitmask,

    /// PID parameters, pulled from the BSP by default but user-modifiable
    pid_config: PidConfig,

    /// Records details on the first sensor read failures since the thermal loop
    /// entered the `Uncontrollable` state and the system was powered off.
    ///
    /// This value is copied to `prev_err_blackbox` when the system is
    /// deemed `Uncontrollable` and powered off
    err_blackbox: &'static mut ThermalSensorErrors,

    /// Previous value of `err_blackbox`, copied over at power-down
    prev_err_blackbox: &'static mut ThermalSensorErrors,

    /// Last group PWM control value
    last_pwm: PWMDuty,

    /// Has the fan watchdog been configured yet?
    fan_watchdog_configured: bool,

    /// Tracks the total duration of excursions into the overheated control
    /// regime.
    overheat_timer: Option<OverheatTimer>,
}

/// Represents the state of a temperature sensor, which either has a valid
/// reading or is marked as inactive (due to power state or being missing)
#[derive(Copy, Clone, Debug)]
pub enum TemperatureReading {
    /// Normal reading, timestamped using monotonic system time
    Valid(TimestampedTemperatureReading),

    /// This sensor is not used in the current power state
    Inactive,
}

/// Represents a temperature reading at the time at which it was taken
#[derive(Copy, Clone, Debug)]
pub struct TimestampedTemperatureReading {
    pub time_ms: u64,
    pub value: Celsius,
}

/// Represents a worst-case temperature reading from the thermal model,
/// including the estimated temperature and the time since the last actual
/// sensor reading (lag).
#[derive(Copy, Clone, PartialEq)]
pub(crate) struct WorstCaseTemperature {
    /// The worst-case temperature estimate from the thermal model, projected
    /// from the `last_reading`.
    worst_case_temp: Celsius,
    /// The last actual temperature reading from the device.
    ///
    /// Subtracting this value from `worst_case_temp` gives the portion of the
    /// worst case temperature that was calculated based on the lag since the
    /// last actual reading fro mthe sensor .
    last_reading: Celsius,
    /// Approximately how old (in seconds) is the the last real temperature?
    age_s: f32,
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
    fn worst_case(
        &self,
        now_ms: u64,
        model: &ThermalProperties,
    ) -> WorstCaseTemperature {
        // How long has it been since the last real life temperature reading?
        let age_s = now_ms.saturating_sub(self.time_ms) as f32 / 1000.0;
        let worst_case_temp =
            Celsius(self.value.0 + age_s * model.temperature_slew_deg_per_sec);
        WorstCaseTemperature {
            worst_case_temp,
            last_reading: self.value,
            age_s,
        }
    }
}

/// Configuration for a PID controller
#[derive(Copy, Clone)]
pub struct PidConfig {
    pub zero: f32,
    pub gain_p: f32,
    pub gain_i: f32,
    pub gain_d: f32,
    pub min_output: f32,
    pub max_output: f32,
}

/// Represents a PID controller that can only push in one direction (i.e. the
/// output must always be positive).
struct OneSidedPidState {
    /// Previous error (if known), for calculating derivative term
    prev_error: Option<f32>,

    /// Accumulated integral term, pre-multiplied by gain
    integral: f32,
}

impl OneSidedPidState {
    /// Attempts to drive the error to zero.
    ///
    /// The error and output are expected to have the same signs, i.e. a large
    /// positive error will produce a large positive output.
    fn run(&mut self, cfg: &PidConfig, error: f32) -> f32 {
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

        // To prevent integral windup, the integral term needs to be clamped to
        // values can affect the output.
        let out_pd = cfg.zero + p_contribution + d_contribution;
        let (integral_min, integral_max) = if out_pd > cfg.max_output {
            (-out_pd, 0.0)
        } else if out_pd < 0.0 {
            (0.0, -out_pd + cfg.max_output)
        } else {
            (-out_pd, cfg.max_output - out_pd)
        };
        // f32::clamp is not inlining well as of 2024-04 so we do it by hand
        // here and below.
        self.integral = self.integral.max(integral_min).min(integral_max);

        // Clamp output values to valid range.
        let out = out_pd + self.integral;
        // same issue with f32::clamp (above)
        out.max(cfg.min_output).min(cfg.max_output)
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
///
/// All of our temperature arrays contain, in order
/// - I2C temperature inputs (read by this task)
/// - Dynamic temperature inputs (read by another task and passed in)
///
/// Note that the canonical temperatures are stored in the `sensors` task; we
/// copy them into these arrays for local operations.
///
/// ## Theory of Operation
///
/// The thermal loop operates in two separate control regimes:
///
/// - **Normal control**, represented by [`ThermalControlState::Running`]; in
///   which fan PWM duty cycles are set by PID control, and,
///
/// - **Overheat**, represented by [`ThermalControlState::Overheat`] and
///   [`ThermalControlState::FanParty`], in which fans are driven at the
///   maximum PWM duty cycle until the system returns to the normal control
///   regime.
///
/// By design, the system should spend most of its time in the normal PID
/// control regime under normal operating conditions.  The overheat control
/// regime is an emergency failsafe mode which is entered only when PID control
/// fails to maintain safe operating temperatures.
///
/// Transitions between these control regimes are governed by the temperature
/// thresholds for components monitored by the thermal control loop, which are
/// configured by a [`ThermalProperties`] struct for each input channel in the
/// BSP.  In particular, each component has a [target] (or _nominal_)
/// temperature threshold, a [critical] temperature, and a [power-down]
/// temperature.  If any monitored component's temperature exceeds its critical
/// threshold, we abandon normal abandon PID control and transition to the
/// overheat control regime.  While in the overheat regime, we drive the fans
/// at 100% PWM duty cycle until all monitored temperatures return to nominal
/// ranges for that component.  Once every component is below its nominal
/// threshold, we return to normal control.
///
/// In addition, the thermal control loop will perform an emergency power down
/// of the system if any component temperature exceeds its power-down threshold.
/// In that case, we will decide that the system's temperatures cannot be
/// controlled, and transition to [`ThermalControlState::Uncontrollable`]. In
/// this state, the thermal loop will request a power state change to A2,
/// shutting down the system.
///
/// - `Overheat`, in which at least one component is critical
/// - `FanParty`, in which all temperatures are below critical, and we will run
///   the fans at 100% duty cycle until we return to nomal
///
/// This diagram depicts the transitions between control states:
///
/// ```text
///  [ BOOT ]
///     |
///     V
/// +---------------+
/// | RUNNING       |<-----------------<-----------------+
/// | (PID control) |                                    |
/// +---------------+                                    |
///    |   |                                             ^
///    |   * . . Any temp                                |
///    |   |     over critical                           * . all temps
///    |   |                                             |   nominal
///    |   |          Overheat control regime            |
///    |   |          (100% PWM duty cycle)              |
///    |   |         . . . . . . . . . . . . .           |
///    |   |         .      +----------+     .           |
///    |   +--------------->|          |--------->-------+
///    +------<-------------| OVERHEAT |     .           |
///    |             .      |          |     .           |
///    |             .      +----------+     .           |
///    |             .        |    ^         .           ^
///    |       all temps      |    * . any temp          |
///    |       under crit . . *    |   over crit         |
///    |             .        |    |         .           |
///    |             .        v    |         .           |
///    |             .     +-----------+     .           |
///    +-------------------| FAN PARTY |----------->-----+
///    |             .     +-----------+     .
///    |             .........................
///    |
///    * . . Any temp over
///    |     power_down
///    |
///    v
/// +----------------+
/// | UNCONTROLLABLE |
/// +----------------+
///    |
///    V
/// [ POWER DOWN ]
/// ```
///
/// [`overheat_timeout_ms`]: ThermalControl#structfield.overheat_timeout_ms
enum ThermalControlState {
    //
    // === Normal control regime states ===
    //
    /// Wait for each sensor to report in at least once
    ///
    /// (dynamic sensors must report in *if* they are present, i.e. not `None`
    /// in the `dynamic_inputs` array)
    Boot,

    /// Normal happy control loop
    Running { pid: OneSidedPidState },

    //
    // === Overheated control regime states ===
    //
    /// In the critical state, one or more components has entered their
    /// critical temperature ranges.  We turn on fans at high power and record
    /// the time at which we entered this state.
    Critical {
        /// The time at which we transitioned to the `Critical` state *this*
        /// time, either from `Running` or from FAN PARTY!!!.
        start_time: u64,
    },

    /// If we are in the `Critical` state and all temperatures drop below their
    /// Critical threshold, but above their nominal threshold, we leave the
    /// `Critical` state and enter FAN PARTY!!!!, a special state that's kind of
    /// halfway between `Critical` and normal operation. In FAN PARTY MODE, we
    /// continue to run the fans at their max duty cycle until we go below a
    /// nomal threshold.
    ///
    /// This gives us an opportunity to recover from overheating by running the
    /// fans aggressively without also deciding to give up and kill ourselves
    /// while things are improving but not fast enough.
    FanParty,

    /// The system cannot control the temperature; power down and wait for
    /// intervention from higher up the stack.
    Uncontrollable,
}

enum ControlResult {
    Pwm(PWMDuty),
    PowerDown,
}

struct OverheatTimer {
    start_time: u64,
    critical_ms: u64,
}

impl<'a> ThermalControl<'a> {
    /// Constructs a new `ThermalControl` based on a `struct Bsp`. This
    /// requires that every BSP has the same internal structure,
    ///
    /// # Panics
    /// This function can only be called once, because it claims mutable static
    /// buffers.
    pub fn new(
        bsp: &'a mut Bsp,
        i2c_task: TaskId,
        sensor_api: SensorApi,
    ) -> Self {
        use static_cell::ClaimOnceCell;

        let [err_blackbox, prev_err_blackbox] = {
            static BLACKBOXEN: ClaimOnceCell<[ThermalSensorErrors; 2]> =
                ClaimOnceCell::new([ThermalSensorErrors::new(); 2]);
            BLACKBOXEN.claim()
        };
        let pid_config = bsp.pid_config;

        Self {
            bsp,
            i2c_task,
            sensor_api,
            target_margin: Celsius(0.0f32),
            state: ThermalControlState::Boot,
            pid_config,

            power_mode: PowerBitmask::empty(), // no sensors active

            last_pwm: PWMDuty(0),

            err_blackbox,
            prev_err_blackbox,
            fan_watchdog_configured: false,
            overheat_timer: None,
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
        if let ThermalControlState::Running { pid, .. } = &mut self.state
            && i == 0.0
        {
            pid.integral = 0.0;
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
        self.bsp.reset_all_values();
        self.state = ThermalControlState::Boot;
        ringbuf_entry!(Trace::AutoState(self.get_state()));
    }

    /// Get latest fan presence state
    pub fn update_fan_presence(&mut self) {
        // Try to configure the fan watchdog, if not yet configured
        //
        // With its longest timeout of 30 seconds, this is longer than it takes
        // to flash on Gimlet -- and right on the edge of how long it takes to
        // dump. On some platforms and/or under some conditions, "humility dump"
        // might be able to induce the watchdog to kick, which may induce a
        // flight-or-fight reaction for whomever is near the fans when they
        // blast off...
        if !self.fan_watchdog_configured {
            match self.set_watchdog(I2cWatchdog::ThirtySeconds) {
                Ok(()) => {
                    ringbuf_entry!(Trace::SetFanWatchdogOk);
                    self.fan_watchdog_configured = true;
                }
                Err(e) => ringbuf_entry!(Trace::SetFanWatchdogError(e)),
            }
        }

        let result = self.bsp.update_fan_presence(
            |fan| ringbuf_entry!(Trace::FanAdded(*fan)),
            |fan| ringbuf_entry!(Trace::FanRemoved(*fan)),
        );

        if let Err(e) = result {
            ringbuf_entry!(Trace::FanPresenceUpdateFailed(e));
        }
    }

    /// Reads all temperature and fan RPM sensors, posting their results
    /// to the sensors task API.
    ///
    /// Records failed sensor reads and failed posts to the sensors task in
    /// the local ringbuf.  In addition, records the first few failed sensor
    /// read in `self.err_blackbox` for later investigation.
    pub fn read_sensors(&mut self) {
        // Read fan data and log it to the sensors task
        self.bsp.read_fan_rpms(
            |id, value| self.sensor_api.post_now(*id, value),
            |id, error| {
                ringbuf_entry!(Trace::FanReadFailed(*id, error));
                self.err_blackbox.push(*id, error);
                self.sensor_api.nodata_now(*id, error.into())
            },
            |id| {
                self.sensor_api
                    .nodata_now(*id, task_sensor_api::NoData::DeviceNotPresent)
            },
        );

        // Read miscellaneous temperature data and log it to the sensors task
        self.bsp.read_misc_sensors(
            self.i2c_task,
            |id, value| self.sensor_api.post_now(*id, value),
            |id, error| {
                ringbuf_entry!(Trace::MiscReadFailed(*id, error));
                self.err_blackbox.push(*id, error);
                self.sensor_api.nodata_now(*id, error.into())
            },
        );

        // We read the power mode right before reading sensors, to avoid
        // potential TOCTOU issues; some sensors cannot be read if they are not
        // powered.
        let power_mode = self.bsp.power_mode();
        self.bsp.read_inputs(
            power_mode,
            self.i2c_task,
            |id, value| self.sensor_api.post_now(*id, value),
            |s, error| {
                // Record an error errors if the sensor is not removable
                // or we get a unexpected error from a removable sensor
                ringbuf_entry!(Trace::SensorReadFailed(
                    s.sensor.sensor_id,
                    error
                ));
                self.err_blackbox.push(s.sensor.sensor_id, error);
                self.sensor_api.nodata_now(s.sensor.sensor_id, error.into())
            },
            |s, error| {
                self.sensor_api.nodata_now(s.sensor.sensor_id, error.into())
            },
            |id| {
                // If the device isn't supposed to be on in the current power
                // state, then record it as Off in the sensors task.
                self.sensor_api
                    .nodata_now(*id, task_sensor_api::NoData::DeviceOff)
            },
        );

        // Note that this function does not send data about dynamic temperature
        // inputs to the `sensors` task!  This is because we don't know what
        // they are, so someone else has to do that.
    }

    /// An extremely simple thermal control loop.
    ///
    /// Returns an error if the control loop failed to read critical sensors;
    /// the caller should set us to some kind of fail-safe mode if this
    /// occurs.
    fn run_control_inner(
        &mut self,
        now_ms: u64,
    ) -> Result<ControlResult, ThermalError> {
        // When the power mode changes, we may require a new set of sensors to
        // be online.  Reset the control state, waiting for all newly-required
        // sensors to come online before re-entering the control loop.
        let prev_power_mode = self.power_mode;
        self.power_mode = self.bsp.power_mode();
        if prev_power_mode != self.power_mode {
            ringbuf_entry!(Trace::PowerModeChanged(self.power_mode));
            self.reset_state();
        }

        // `input` sensors have all been read during `read_sensors`.

        // The dynamic inputs don't depend on power mode; instead, they are
        // assumed to be present when a model exists in `self.dynamic_inputs`;
        // this model is set by external callers using
        // `update_dynamic_input` and `remove_dynamic_input`.
        //
        // TODO(AJM): Should we be doing this in `read_sensors` instead of
        // `run_control`?
        self.bsp
            .read_dynamic_inputs_back_from_sensor_api(&self.sensor_api);

        // Run a common analysis pass first, regardless of state. Don't take any
        // side effectful actions yet though.
        let mut all_nominal = true;
        let mut any_power_down = None;
        let mut any_critical = None;
        let mut worst_margin = f32::MAX;

        // Remember, positive margin means that all parts are happily
        // below their max temperature; negative means someone is
        // overheating.  We want to pick the _smallest_ margin, since
        // that's the part which is most overheated.
        let f = |sensor_id, v, model| {
            if let TemperatureReading::Valid(v) = v {
                let worst_case = v.worst_case(now_ms, &model);
                let temperature = worst_case.worst_case_temp;
                all_nominal &= model.is_nominal(temperature);
                if model.should_power_down(temperature) {
                    any_power_down = Some((sensor_id, worst_case));
                }
                if model.is_critical(temperature) {
                    any_critical = Some((sensor_id, worst_case));
                }
                worst_margin = worst_margin.min(model.margin(temperature).0);
            }

            // Hidden assumption here! ALL `TemperatureReading`s here will be
            // valid, UNLESS:
            //
            // 1. We are in Boot state and we allow missing inputs, which will
            //    skip anything we haven't heard yet (but will report `false`
            //    for `bsp.all_inputs_present()`)
            // 2. For input sensors: the given input is not active in this
            //    power state
            // 3. For dynamic sensors: The given dynamic input has not been
            //    given to us to activate
            //
            // We don't necessarily *check* that is the case though, and we
            // might want to in the for_each_temp(_allow_missing_inputs)
            // functions!
        };
        match self.state {
            ThermalControlState::Boot => {
                self.bsp.for_each_temp_allow_missing_inputs(f)
            }
            ThermalControlState::Running { .. } => self.bsp.for_each_temp(f),
            ThermalControlState::Critical { .. } => self.bsp.for_each_temp(f),
            ThermalControlState::FanParty => self.bsp.for_each_temp(f),
            ThermalControlState::Uncontrollable => {
                return Ok(ControlResult::PowerDown);
            }
        }

        // In any state, if we've reached the "any_power_down" threshold, then
        // it's time to go.
        if let Some(due_to) = any_power_down {
            return Ok(self.transition_to_uncontrollable_due_to(due_to, now_ms));
        }

        let all_nominal = all_nominal;
        let any_critical = any_critical;
        let worst_margin = worst_margin;

        // TODO(AJM): I think we could dedupe some of the code below, basically
        // working backwards and checking if we "qualify" for each state, though
        // that's a bit more invasive of a change
        Ok(match &mut self.state {
            ThermalControlState::Boot => {
                if self.bsp.all_inputs_present() {
                    self.transition_to_running(worst_margin, now_ms)
                } else {
                    ControlResult::Pwm(PWMDuty(
                        self.pid_config.max_output as u8,
                    ))
                }
            }
            ThermalControlState::Running { pid } => {
                if let Some(due_to) = any_critical {
                    self.transition_to_critical(due_to, now_ms)
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
                    );
                    ControlResult::Pwm(PWMDuty(pwm as u8))
                }
            }
            ThermalControlState::Critical { .. } => {
                if all_nominal {
                    self.transition_to_running(worst_margin, now_ms)
                } else if !any_critical.is_some() {
                    // If all temperatures have gone below critical, but are
                    // still above nominal, stop the overheat timeout but
                    // continue running at 100% PWM until things go below
                    // nominal.
                    self.transition_to_fan_party(now_ms)
                } else {
                    ControlResult::Pwm(PWMDuty(
                        self.pid_config.max_output as u8,
                    ))
                }
            }
            ThermalControlState::FanParty => {
                if let Some(due_to) = any_critical {
                    // If anything's gone over critical, transition back to the
                    // `Critical` state.
                    self.transition_to_critical(due_to, now_ms)
                } else if all_nominal {
                    self.transition_to_running(worst_margin, now_ms)
                } else {
                    ControlResult::Pwm(PWMDuty(
                        self.pid_config.max_output as u8,
                    ))
                }
            }
            ThermalControlState::Uncontrollable => ControlResult::PowerDown,
        })
    }

    /// An extremely simple thermal control loop.
    ///
    /// Returns an error if the control loop failed to read critical sensors;
    /// the caller should set us to some kind of fail-safe mode if this
    /// occurs.
    pub fn run_control(&mut self) -> Result<(), ThermalError> {
        let now_ms = sys_get_timer().now;
        let control_result = self.run_control_inner(now_ms)?;
        match control_result {
            ControlResult::Pwm(target_pwm) => {
                // Send the new RPM to all of our fans
                ringbuf_entry!(Trace::ControlPwm(target_pwm.0));
                self.set_pwm(Ok(target_pwm), now_ms)
            }
            ControlResult::PowerDown => {
                ringbuf_entry!(Trace::PowerDownAt(sys_get_timer().now));
                *self.prev_err_blackbox = *self.err_blackbox;
                self.err_blackbox.clear();
                if let Err(e) = self.bsp.power_down() {
                    ringbuf_entry!(Trace::PowerDownFailed(e));
                }
                self.set_pwm(Err(task_sensor_api::NoData::DeviceOff), now_ms)?;
                Ok(())
            }
        }
    }

    /// Transition the control state to the normal control regime.
    ///
    /// This sets the state to `Running`, and performs a single iteration of the
    /// PID control loop to determine the new duty cycle.
    fn transition_to_running(
        &mut self,
        worst_margin: f32,
        now_ms: u64,
    ) -> ControlResult {
        self.record_leaving_critical(now_ms);
        self.record_leaving_overheat(now_ms);

        // Transition to the Running state and run a single
        // iteration of the PID control loop.
        let mut pid = OneSidedPidState::default();
        let pwm =
            pid.run(&self.pid_config, self.target_margin.0 - worst_margin);
        self.state = ThermalControlState::Running { pid };
        ringbuf_entry!(Trace::AutoState(self.get_state()));

        ControlResult::Pwm(PWMDuty(pwm as u8))
    }

    /// Transition the control state to `Critical`, in response to a
    /// component exceeding its critical threshold.
    fn transition_to_critical(
        &mut self,
        (sensor_id, worst_case): (SensorId, WorstCaseTemperature),
        now_ms: u64,
    ) -> ControlResult {
        let WorstCaseTemperature {
            worst_case_temp,
            last_reading,
            age_s,
        } = worst_case;
        ringbuf_entry!(Trace::CriticalDueTo {
            sensor_id,
            worst_case_temp
        });
        ringbuf_entry!(Trace::LastRealTemperature {
            sensor_id,
            temperature: last_reading,
            age_s,
        });
        self.state = ThermalControlState::Critical { start_time: now_ms };
        ringbuf_entry!(Trace::AutoState(self.get_state()));
        if self.overheat_timer.is_none() {
            self.overheat_timer = Some(OverheatTimer {
                start_time: now_ms,
                critical_ms: 0,
            })
        }

        ControlResult::Pwm(PWMDuty(self.pid_config.max_output as u8))
    }

    /// Transition the control state to `FanParty` (from `Critical`), in
    /// response to all component temperatures dropping below their critical
    /// thresholds.
    fn transition_to_fan_party(&mut self, now_ms: u64) -> ControlResult {
        self.record_leaving_critical(now_ms);
        self.state = ThermalControlState::FanParty;
        ringbuf_entry!(Trace::AutoState(self.get_state()));

        // It's PARTY TIME!!!!
        ControlResult::Pwm(PWMDuty(self.pid_config.max_output as u8))
    }

    /// Transition to the `Uncontrollable` state due to a device exceeding its
    /// power-down temperature threshold.
    ///
    /// This is a wrapper around [`Self::transition_to_uncontrollable`] which
    /// also records the sensor ID and temperature measurements for the device
    /// that tripped over the threshold. We separate this into two functions as
    /// we may also transition to uncontrollable due to an inability to read
    /// sensors at all.
    fn transition_to_uncontrollable_due_to(
        &mut self,
        (sensor_id, worst_case): (SensorId, WorstCaseTemperature),
        now_ms: u64,
    ) -> ControlResult {
        let WorstCaseTemperature {
            worst_case_temp,
            last_reading,
            age_s,
        } = worst_case;
        ringbuf_entry!(Trace::PowerDownDueTo {
            sensor_id,
            worst_case_temp
        });
        ringbuf_entry!(Trace::LastRealTemperature {
            sensor_id,
            temperature: last_reading,
            age_s,
        });
        self.transition_to_uncontrollable(now_ms)
    }

    /// Transition to the `Uncontrollable` state, either in response to thermal
    /// sensor errors, or a component exceeding its power-down temperature
    /// threshold.
    fn transition_to_uncontrollable(&mut self, now_ms: u64) -> ControlResult {
        self.record_leaving_critical(now_ms);
        self.record_leaving_overheat(now_ms);

        self.bsp.reset_all_values();
        self.state = ThermalControlState::Uncontrollable;
        ringbuf_entry!(Trace::AutoState(self.get_state()));

        ControlResult::PowerDown
    }

    /// Record leaving the `Critical` state. This includes both transitions
    /// between `Critical` and `FanParty` (in which case we remain in the
    /// overheated control regime), and transitions from `Critical` back to
    /// `Running` or `Uncontrollable`.
    fn record_leaving_critical(&mut self, now_ms: u64) {
        if let ThermalControlState::Critical { start_time, .. } = self.state
            && let Some(OverheatTimer {
                ref mut critical_ms,
                ..
            }) = self.overheat_timer
        {
            *critical_ms =
                critical_ms.saturating_add(now_ms.saturating_sub(start_time));
        }
    }

    /// Record leaving the overheated control regime. This is *not* called on
    /// transitions between the `Critical` and `FanParty` states, in which we
    /// remain within the overheated control regime.
    fn record_leaving_overheat(&mut self, now_ms: u64) {
        if let Some(OverheatTimer {
            start_time,
            critical_ms,
        }) = self.overheat_timer.take()
        {
            // TODO(eliza): stash a "last overheat durations" someplace that we
            // can query it, even if it's fallen off the ringbuf?
            // TODO(eliza): ereport?
            ringbuf_entry!(Trace::OverheatedFor(
                now_ms.saturating_sub(start_time)
            ));
            ringbuf_entry!(Trace::CriticalFor(critical_ms));
        }
    }

    /// Attempts to set the PWM duty cycle of every fan in this group.
    ///
    /// For fans that are present, set to `pwm`. For fans that are not present,
    /// set to zero. Returns the last error if one occurred, but does not short
    /// circuit (i.e. attempts to set *all* present fan duty cycles, even if one
    /// fails)
    ///
    /// The PWM value (or error code) is sent to the `sensors` task for logging,
    /// timestamped with the `now_ms` argument.
    pub fn set_pwm(
        &mut self,
        pwm: Result<PWMDuty, task_sensor_api::NoData>,
        now_ms: u64,
    ) -> Result<(), ThermalError> {
        // We'll post the PWM value to the sensors task for logging
        use task_sensor_api::config::other_sensors;
        pub const OUTPUT_PWM_SENSOR: SensorId =
            other_sensors::THERMAL_LOOP_FAN_CTRL_PWM_SENSOR;
        let pwm = match pwm {
            Ok(pwm) => {
                if pwm.0 > 100 {
                    self.sensor_api.nodata(
                        OUTPUT_PWM_SENSOR,
                        task_sensor_api::NoData::DeviceError,
                        now_ms,
                    );
                    return Err(ThermalError::InvalidPWM);
                }
                self.sensor_api
                    .post(OUTPUT_PWM_SENSOR, pwm.0 as f32, now_ms);
                pwm
            }
            Err(e) => {
                self.sensor_api.nodata(OUTPUT_PWM_SENSOR, e, now_ms);
                PWMDuty(0)
            }
        };
        self.last_pwm = pwm;
        self.bsp.set_all_fan_rpms(pwm)
    }

    /// Attempts to set the PWM of every fan to whatever the previous value was.
    ///
    /// This is used by ThermalMode::Manual to accomodate the removal and
    /// replacement of fan modules.
    pub fn maintain_pwm(&mut self) -> Result<(), ThermalError> {
        self.set_pwm(Ok(self.last_pwm), sys_get_timer().now)
    }

    pub fn set_watchdog(
        &mut self,
        wd: I2cWatchdog,
    ) -> Result<(), ThermalError> {
        let mut result = Ok(());

        self.bsp.for_each_fctrl(|fctrl| {
            if fctrl.set_watchdog(wd).is_err() {
                result = Err(ThermalError::DeviceError);
            }
        })?;

        result
    }

    pub fn get_state(&self) -> ThermalAutoState {
        match self.state {
            ThermalControlState::Boot => ThermalAutoState::Boot,
            ThermalControlState::Running { .. } => ThermalAutoState::Running,
            ThermalControlState::Critical { .. } => ThermalAutoState::Critical,
            ThermalControlState::Uncontrollable => {
                ThermalAutoState::Uncontrollable
            }
            ThermalControlState::FanParty => ThermalAutoState::FanParty,
        }
    }

    pub fn update_dynamic_input(
        &mut self,
        index: usize,
        model: ThermalProperties,
    ) -> Result<(), ThermalError> {
        // If we're adding a new dynamic input, then reset the state to `Boot`,
        // ensuring that we'll wait for that channel to provide us with at least
        // one valid reading before resuming the PID loop.
        //
        // TODO(AJM): We just ignore it if there was already a dynamic input
        // there already?
        let is_new = self.bsp.update_dynamic_input(index, model)?;
        if is_new {
            ringbuf_entry!(Trace::AddedDynamicInput(index));
            self.reset_state();
        }
        Ok(())
    }

    pub fn remove_dynamic_input(
        &mut self,
        index: usize,
    ) -> Result<(), ThermalError> {
        let sensor_id = self.bsp.remove_dynamic_input(index)?;
        ringbuf_entry!(Trace::RemovedDynamicInput(index));

        // Post this reading to the sensors task as well
        self.sensor_api
            .nodata_now(sensor_id, task_sensor_api::NoData::DeviceNotPresent);
        Ok(())
    }
}

pub fn unexpected_failure(s: &InputChannel, e: SensorReadError) -> bool {
    let removable = matches!(
        s.ty,
        ChannelType::Removable | ChannelType::RemovableAndErrorProne
    );
    let removed = e == SensorReadError::I2cError(ResponseCode::NoDevice);
    !(removable && removed)
}
