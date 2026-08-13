// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! # Thermal Control Loop
//!
//! This module contains the core logic of the thermal control loop of hubris
//! devices. This code has several core responsibilities:
//!
//! 1. Monitoring "inputs", which are temperature readings present on the
//!    device, either directly (by querying external i2c devices), or indirectly
//!    (by querying the sensor task). See definitions of terms below for a more
//!    exacting explanation.
//! 2. Controlling "outputs", which are PWM commanded fan outputs, in order to
//!    maintain reasonable temperatures of components of the device.
//! 3. Monitoring for any inputs reaching a "critical" temperature threshold at
//!    which the device should be powered off to avoid component damage.
//!
//! Responsibilities for the above are broken into two main entities:
//!
//! 1. The "BSP", or "Board Support Package", which is specific to each hardware
//!    assembly. The BSP maintains the list of present inputs, outputs, and
//!    parameters specific to the board. The BSP is responsible for maintaining
//!    any necessary state for the components it queries or commands. The BSP
//!    encapsulates any implementation-specific operation and behaviors,
//!    including how to communicate with physical sensors.
//! 2. The control loop, which is contained in this file. The control loop is
//!    agnostic to device-specific details, and is common to all hubris devices.
//!    The control loop is responsible for sequencing of behaviors, as well as
//!    determining the overall state and response behavior of the system.
//!
//! The control loop operates in one of two primary modes:
//!
//! 1. "Manual", where sensors will be polled, but fan output is maintained at
//!    a specifically commanded level. This is typically used only for
//!    development purposes.
//! 2. "Automatic", where inputs are checked against their nominal temperature
//!    levels. In this mode, each sensor's "margin", or level above their
//!    nominal temperature, is tracked, and the highest margin is fed into a
//!    PID control algorithm to determine the necessary fan response necessary
//!    to return that input to acceptable levels.
//!
//! ## Important terms
//!
//! * Fans: The fans of a system, consisting of both the output controlled in
//!   PWM duty cycle percentages, as well as a sensor monitoring the measured
//!   RPM of the fans.
//! * Inputs: Temperature sensors that are actively polled by the thermal
//!   control loop. Typically I2C based. This includes both permananently
//!   attached sensors as well as removable sensors. Some Inputs may only be
//!   active in a subset of power states. When present and active, the data from
//!   these inputs are used as part of the PID control loop. The readings from
//!   these sensors are also reported to the Sensors API.
//! * Misc Sensors: Temperature sensors that are actively polled by the thermal
//!   control loop and reported to the Sensors API, but are not used as inputs
//!   to the PID control loop. Misc sensors are active in all power states.
//!     * Example: The six TMP117 air sensors on Cosmo, which monitor the
//!       ambient air temperature within the sled.
//! * Dynamic Inputs: Temperature sensors that are NOT actively polled by the
//!   thermal control loop, from which readings are instead obtained by querying
//!   the sensor API. These readings are used as inputs to the PID control loop.
//!   By default, all Dynamic Inputs are not marked as present, and require an
//!   external command (via IPC) to provide the necessary thermal model, and
//!   inform the control loop that the sensors are active and should be queried.
//!     * Example: Transceivers (xcvrs) 0..32 on Sidecar, which are managed by
//!       the `transceivers-server` task, which monitors when an xcvr has been
//!       added and removed, and monitors the temperature of any present xcvr.
//! * Watchdogs: Features of the external fan controllers that automatically
//!   move the fans to their highest commanded speed when not communicated with
//!   for a configured time duration.

use crate::{ThermalError, Trace, bsp::PowerBitmask};
use drv_i2c_devices::max31790::I2cWatchdog;

use microcbor::Encode;
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use task_packrat_api::Packrat;
use task_sensor_api::{NoData, Sensor as SensorApi, SensorId};
use task_thermal_api::{
    FanProperties, SensorReadError, ThermalAutoState, ThermalProperties,
};
use userlib::{
    sys_get_timer,
    units::{Celsius, PWMDuty, Rpm},
};

////////////////////////////////////////////////////////////////////////////////

/// The platform/bsp specific interface contract for the thermal control loop.
///
/// This interface defines how the control loop below perceives and controls
/// the device environment it inhabits.
pub trait BspInterface {
    /// Default [`PidConfig`] to use when in automatic control mode
    const PID_CONFIG: PidConfig;

    /// Run the PID loop on startup
    const USE_CONTROLLER: bool;

    /// BSP-specific fan identifier
    type FanBspId;

    /// Instruct the sequencer to power down the system
    fn power_down(&self) -> Result<(), crate::SeqError>;

    /// The current power mode reported by the sequencer
    fn power_mode(&self) -> PowerBitmask;

    /// Poll every fan in the system, then yield each one for reporting.
    ///
    /// Implementations must update the presence of every removable fan using
    /// [`Fan::update_presence()`], and poll the RPM of every fan using
    /// [`Fan::poll_rpm_with()`]. The returned iterator should then yield all
    /// fans, including any that are not present or are in a deviant state.
    fn poll_fan_rpms(
        &mut self,
    ) -> impl Iterator<Item = &'_ mut Fan<Self::FanBspId>>;

    /// Return an iterator of the current status of each misc sensor. The
    /// iterator may be lazy, meaning that failure to exhaust the iterator means
    /// that not all sensors will be actively queried.
    fn poll_misc_sensors(
        &self,
    ) -> impl Iterator<Item = MiscSensorPollingOutcome>;

    /// Return an iterator of the outcome of polling each input. The iterator
    /// reports whether the input is powered, present, and whether the latest
    /// query was successful. This updates the state of the sensor, which is
    /// obtained by calling [`Self::all_active_inputs()`]. Unlike this API,
    /// that function retains the latest valid value in case of transient
    /// read failures.
    fn poll_inputs(
        &mut self,
        mode: PowerBitmask,
    ) -> impl Iterator<Item = InputPollingOutcome>;

    /// Updates the BSP-maintained state for all all dynamic inputs that are
    /// currently marked as present. The information from this querying can be
    /// obtained by calling [`Self::all_active_inputs()`].
    fn poll_dynamic_inputs(&mut self, sensor_api: &task_sensor_api::Sensor);

    /// Set the given dynamic input as present, and configures it with the given
    /// model.
    ///
    /// Returns `Ok(true)` when the input was not previously present. Returns
    /// `Ok(false)` if the input was already present and the new model was
    /// ignored. Returns an error if the given index was invalid.
    fn register_dynamic_input(
        &mut self,
        index: usize,
        model: ThermalProperties,
    ) -> Result<bool, ThermalError>;

    /// Set the given dynamic as not present.
    ///
    /// Returns Ok if the given index was valid, otherwise returns an error.
    /// Does not indicate whether the input was previously present or not.
    fn remove_dynamic_input(
        &mut self,
        index: usize,
    ) -> Result<SensorId, ThermalError>;

    /// Have all powered inputs (regular and dynamic) been queried?
    ///
    /// This is used to determine whether it is appropriate to leave the `Boot`
    /// state. A sensor is considered to have been queried if it is:
    ///
    /// * Unpowered
    /// * Not present and marked as removable
    /// * Has ever received a valid reply (even if not currently responding)
    fn all_inputs_queried(&self) -> bool;

    /// Visit all temperature sensors, first the inputs, then the dynamic
    /// inputs. Only yields inputs that are powered, present (if removable), and
    /// have ever received a valid reading. [`Self::all_inputs_queried()`]
    /// should be used to determine if all inputs necessary to leave the Boot
    /// state are present.
    ///
    /// This function reflects the states polled by [`Self::poll_inputs()`] and
    /// [`Self::poll_dynamic_inputs()`].
    fn all_active_inputs(&self) -> impl Iterator<Item = ActiveInputState<'_>>;

    /// For any input that has received a valid reading, mark it as not
    /// received.
    fn reset_all_values(&mut self);

    /// Set all fan controller watchdogs to the given duration
    fn set_all_watchdogs(
        &mut self,
        watchdog: I2cWatchdog,
    ) -> Result<(), ThermalError>;

    /// Attempt to set all fan outputs to the given duty cycle. If a fan is
    /// removable and not present set the duty to 0. Attempts to set ALL duty
    /// cycles, even if some setting operations fail. In case of any failures,
    /// the most recent error is returned.
    fn set_all_fan_duty(&mut self, duty: PWMDuty) -> Result<(), ThermalError>;
}

////////////////////////////////////////////////////////////////////////////////

/// State of a given fan
#[derive(Copy, Clone, PartialEq)]
pub enum FanState {
    NotPresent,
    Present(FanPresentState),
}

/// State specific to fans that are present
#[allow(dead_code)] // Not all bsps have fans!
#[derive(Copy, Clone, PartialEq)]
pub enum FanPresentState {
    /// The fan is physically present, but is unresponsive to RPM queries
    Unresponsive(SensorReadError),
    /// The fan is present and at a reasonable speed
    Nominal(Rpm),
    /// The fan is present, but is overspeed
    TooFast(Rpm),
    /// The fan is present, but is underspeed
    TooSlow(Rpm),
}

/// Represents the individual fans in the system
///
/// Depending on the system we have diferent numbers of fans structured in
/// different ways. Not all fans are guaranteed to be there at all times so
/// their corresponding sensor is an `Option`. We should not read the RPM of
/// fans which are not present and their PWM should only be driven low.
#[allow(dead_code)] // Not all bsps have fans!
pub struct Fan<D> {
    /// The sensor_api ID used for reporting fan RPMs
    pub rpm_sensor_id: SensorId,
    /// Have we sent a notice about whether the fan was added or removed yet?
    /// This includes ringbuf and ereport output.
    pub presence_acked: bool,
    /// Have we sent a notice about the state of a present fan yet?
    /// This includes ringbuf and ereport output.
    pub state_acked: bool,
    /// The current state of the fan
    pub cur_state: FanState,
    /// A BSP-specific ID used to identify the fan
    pub bsp_data: D,
    /// Parameter model for this fan
    pub model: FanProperties,
}

#[allow(dead_code)] // Not all bsps have fans!
impl<D> Fan<D> {
    /// Create a new fan
    pub const fn new(
        rpm_sensor_id: SensorId,
        model: FanProperties,
        bsp_data: D,
    ) -> Self {
        Self {
            rpm_sensor_id,
            presence_acked: false,
            state_acked: false,
            cur_state: FanState::NotPresent,
            bsp_data,
            model,
        }
    }

    /// The currently tracked state of the fan
    pub(crate) fn current_state(&self) -> FanState {
        self.cur_state
    }

    /// Is the current fan physically present?
    pub(crate) fn is_present(&self) -> bool {
        !matches!(self.cur_state, FanState::NotPresent)
    }

    /// Mark the fan as present or not.
    ///
    /// This method is used to avoid modifying the current state of the fan
    /// if the presence has not changed. If the fan is newly not present, it
    /// will be marked as such. If the fan is newly present, it will be moved
    /// to the `Present(Unresponsive)` state. Otherwise, the fan state will
    /// not be updated.
    pub(crate) fn update_presence(&mut self, is_present: bool) {
        match (is_present, self.cur_state) {
            (true, FanState::NotPresent) => {
                self.update_state(FanState::Present(
                    FanPresentState::Unresponsive(SensorReadError::NoData),
                ))
            }
            (true, _) => {}
            (false, _) => {
                self.update_state(FanState::NotPresent);
            }
        }
    }

    /// Update the current state of the fan.
    ///
    /// This method is the primary logic for state transitions of the fan.
    /// It is responsible for determining whether new notification is
    /// required.
    pub(crate) fn update_state(&mut self, new: FanState) {
        use FanPresentState as Fps;
        use FanState as Fs;
        match (self.cur_state, new) {
            // Not present -> Not Present, nothing to update
            (Fs::NotPresent, Fs::NotPresent) => {}
            // Presence change, update:
            //
            // - New state
            // - Presence ack state
            // - Status ack state
            (Fs::NotPresent, Fs::Present(_))
            | (Fs::Present(_), Fs::NotPresent) => {
                self.cur_state = new;
                self.presence_acked = false;
                self.state_acked = false;
            }
            // Present -> Present
            (Fs::Present(cur), Fs::Present(newp)) => match (cur, newp) {
                // Same -> Same, just take state
                (Fps::Nominal(_), Fps::Nominal(_))
                | (Fps::TooFast(_), Fps::TooFast(_))
                | (Fps::TooSlow(_), Fps::TooSlow(_))
                | (Fps::Unresponsive(_), Fps::Unresponsive(_)) => {
                    self.cur_state = new;
                }
                // Any of the following:
                //
                // - Nominal -> Deviant
                // - Deviant -> Nominal
                // - Deviant -> Deviant
                //
                // Take:
                //
                // - New state
                // - Status ack state
                (Fps::Nominal(_), _)
                | (_, Fps::Nominal(_))
                | (Fps::TooFast(_), Fps::Unresponsive(_))
                | (Fps::TooFast(_), Fps::TooSlow(_))
                | (Fps::TooSlow(_), Fps::Unresponsive(_))
                | (Fps::TooSlow(_), Fps::TooFast(_))
                | (Fps::Unresponsive(_), Fps::TooFast(_))
                | (Fps::Unresponsive(_), Fps::TooSlow(_)) => {
                    self.cur_state = new;
                    self.state_acked = false;
                }
            },
        }
    }

    /// Update the RPM of a present fan with the given closure, which should
    /// retrieve the RPM. Used to share logic across different fan controllers
    pub(crate) fn poll_rpm_with<E: Into<SensorReadError>>(
        &mut self,
        poll_rpm: impl FnOnce() -> Result<Rpm, E>,
    ) {
        // If this fan is not present, then do not attempt to poll it. Presence
        // is only restored via presence polling.
        if !self.is_present() {
            return;
        }

        // Try to get the RPM reading for this fan
        let res = poll_rpm();
        match res {
            Ok(rpm) => {
                // The poll went well! Use the model to determine if this
                // reading is nominal or not, and report that as the state.
                let state = if rpm < self.model.underspeed_rpm {
                    FanPresentState::TooSlow(rpm)
                } else if rpm > self.model.overspeed_rpm {
                    FanPresentState::TooFast(rpm)
                } else {
                    FanPresentState::Nominal(rpm)
                };
                self.update_state(FanState::Present(state));
            }
            Err(e) => {
                // No good, mark as unresponsive
                self.update_state(FanState::Present(
                    FanPresentState::Unresponsive(e.into()),
                ));
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

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

/// The outcome of [`InputChannel::poll_input()`].
#[allow(dead_code)] // Not all bsps have inputs!
pub enum InputPollingOutcome {
    /// Sensor was not read because the power mode indicated that it would not
    /// be enabled in this state.
    Unpowered { sensor_id: SensorId },
    /// This sensor was missing, but it's okay because it was either Removable
    /// and not there, or Removable and Error Prone and had any kind of error.
    AcceptableMissing {
        sensor_id: SensorId,
        err: SensorReadError,
    },
    /// Any read error that didn't match the "Acceptable" cases listed above.
    UnacceptableMissing {
        sensor_id: SensorId,
        err: SensorReadError,
    },
    /// We read the data! Hooray!
    Success {
        sensor_id: SensorId,
        now: u64,
        value: Celsius,
    },
}

/// Status of a regular or dynamic input
pub struct ActiveInputState<'a> {
    pub sensor_id: SensorId,
    pub reading: &'a TimestampedTemperatureReading,
    pub model: &'a ThermalProperties,
}

/// Outcome of polling a misc sensor
pub struct MiscSensorPollingOutcome {
    pub sensor_id: SensorId,
    pub outcome: Result<Celsius, SensorReadError>,
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
    pub state: DynamicTemperatureState,
}

/// Represents the state of a dynamic temperature sensor (which are added
/// and removed at runtime by IPCs from outside the thermal loop). Such a
/// sensor either has a valid reading or is marked as inactive (due to power
/// state or not having been added to the thermal loop).
#[derive(Copy, Clone, Debug)]
#[allow(dead_code)] // Not all bsps have inputs!
pub enum DynamicTemperatureState {
    /// Device has not been enabled
    Disabled,

    /// The device is powered in the current mode, but has not yet been
    /// queried successfully
    NotYetQueried { model: ThermalProperties },

    /// This device has been queried successfully at least once, and this
    /// contains the most recent valid reply
    ValidAtLeastOnce {
        reading: TimestampedTemperatureReading,
        model: ThermalProperties,
    },
}

#[allow(dead_code)]
impl DynamicInputChannel {
    pub(crate) const fn new(sensor_id: SensorId) -> Self {
        Self {
            sensor_id,
            state: DynamicTemperatureState::Disabled,
        }
    }

    pub(crate) fn has_been_queried(&self) -> bool {
        match self.state {
            // Not queried? No!
            DynamicTemperatureState::NotYetQueried { .. } => false,

            // Either the input is disabled (so we have done all the querying
            // necessary), or it has been valid in the past.
            DynamicTemperatureState::Disabled => true,
            DynamicTemperatureState::ValidAtLeastOnce { .. } => true,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone)]
#[allow(dead_code)] // used only by the debugger
pub struct TimestampedSensorError {
    pub timestamp: u64,
    pub sensor_id: SensorId,
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

    pub fn push(&mut self, sensor_id: SensorId, err: SensorReadError) {
        if let Some(v) = self.values.get_mut(self.next as usize) {
            let timestamp = userlib::sys_get_timer().now;
            *v = Some(TimestampedSensorError {
                sensor_id,
                err,
                timestamp,
            });
            self.next += 1;
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// The thermal control loop.
///
/// This object uses slices of sensors and fans, which must be owned
/// elsewhere; the standard pattern is to create static arrays in a
/// `struct Bsp` which is conditionally included based on board name.
pub(crate) struct ThermalControl<'a, B: BspInterface> {
    /// Reference to board-specific parameters
    bsp: &'a mut B,

    /// Task to which we should post sensor data updates
    sensor_api: SensorApi,

    /// Task to which we should post ereports
    ereporter: Ereporter,

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
#[allow(dead_code)] // Not all bsps have inputs!
pub enum TemperatureReading {
    /// Device is not powered, and has not been read
    Unpowered,

    /// The device is powered in the current mode, but has not yet been
    /// queried successfully
    NotYetQueried,

    /// The device is removable, and has been removed
    Disconnected,

    /// This device has been queried successfully at least once, and this
    /// contains the most recent valid reply
    ValidAtLeastOnce(TimestampedTemperatureReading),
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

impl<'a, B: BspInterface> ThermalControl<'a, B> {
    /// Constructs a new `ThermalControl` based on a `struct Bsp`. This
    /// requires that every BSP has the same internal structure,
    ///
    /// # Panics
    /// This function can only be called once, because it claims mutable static
    /// buffers.
    pub fn new(
        bsp: &'a mut B,
        sensor_api: SensorApi,
        packrat_api: Packrat,
    ) -> Self {
        use static_cell::ClaimOnceCell;

        let [err_blackbox, prev_err_blackbox] = {
            static BLACKBOXEN: ClaimOnceCell<[ThermalSensorErrors; 2]> =
                ClaimOnceCell::new([ThermalSensorErrors::new(); 2]);
            BLACKBOXEN.claim()
        };

        Self {
            bsp,
            sensor_api,
            target_margin: Celsius(0.0f32),
            state: ThermalControlState::Boot,
            pid_config: B::PID_CONFIG,

            power_mode: PowerBitmask::empty(), // no sensors active

            last_pwm: PWMDuty(0),

            err_blackbox,
            prev_err_blackbox,
            fan_watchdog_configured: false,
            overheat_timer: None,
            ereporter: Ereporter::claim_static_resources(packrat_api),
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
        self.pid_config = B::PID_CONFIG;

        // Set the target_margin to 0, indicating no overcooling
        self.target_margin = Celsius(0.0f32);
    }

    /// Resets the control state
    fn reset_state(&mut self) {
        self.bsp.reset_all_values();
        self.state = ThermalControlState::Boot;
        ringbuf_entry!(Trace::AutoState(self.get_state()));
    }

    /// Reads all temperature and fan RPM sensors, posting their results
    /// to the sensors task API.
    ///
    /// Records failed sensor reads and failed posts to the sensors task in
    /// the local ringbuf.  In addition, records the first few failed sensor
    /// read in `self.err_blackbox` for later investigation.
    pub fn read_sensors(&mut self) {
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

        // Read fan data and log it to the sensors task
        let now = sys_get_timer().now;
        for fan in self.bsp.poll_fan_rpms() {
            report_fan_state(fan, &self.sensor_api, now, &mut self.ereporter);
        }

        // Read miscellaneous temperature data and log it to the sensors task
        //
        // We don't retain state for misc sensors, as that is all stored in the
        // sensor task itself. We're just in charge of polling them.
        for outcome in self.bsp.poll_misc_sensors() {
            match outcome.outcome {
                Ok(v) => self.sensor_api.post_now(outcome.sensor_id, v.0),
                Err(e) => {
                    ringbuf_entry!(Trace::MiscReadFailed(outcome.sensor_id, e));
                    self.err_blackbox.push(outcome.sensor_id, e);
                    self.sensor_api.nodata_now(outcome.sensor_id, e.into())
                }
            }
        }

        // We read the power mode right before reading sensors, to avoid
        // potential TOCTOU issues; some sensors cannot be read if they are not
        // powered.
        let power_mode = self.bsp.power_mode();
        for res in self.bsp.poll_inputs(power_mode) {
            match res {
                InputPollingOutcome::Success {
                    sensor_id,
                    now,
                    value,
                } => {
                    self.sensor_api.post(sensor_id, value.0, now);
                }
                InputPollingOutcome::AcceptableMissing { sensor_id, err } => {
                    self.sensor_api.nodata_now(sensor_id, err.into());
                }
                InputPollingOutcome::UnacceptableMissing { sensor_id, err } => {
                    // Record an error if the sensor is not removable, or if
                    // we got an unexpected error from a removable sensor
                    ringbuf_entry!(Trace::SensorReadFailed(sensor_id, err));
                    self.err_blackbox.push(sensor_id, err);
                    self.sensor_api.nodata_now(sensor_id, err.into());
                }
                InputPollingOutcome::Unpowered { sensor_id } => {
                    // If the device isn't supposed to be on in the current
                    // power state, then record it as Off in the sensors task.
                    self.sensor_api.nodata_now(
                        sensor_id,
                        task_sensor_api::NoData::DeviceOff,
                    );
                }
            }
        }

        // Note that this function does not send data about dynamic temperature
        // inputs to the `sensors` task!  This is because we don't know what
        // they are, so someone else has to do that.
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
                self.set_pwm(Err(task_sensor_api::NoData::DeviceOff), now_ms)
            }
        }
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
            // TODO(AJM): the old code would now re-populate the state from the
            // sensor task, while we continue on to now do control with all
            // empty state. This potentially delays us in "Boot" mode for one
            // extra tick which is 1s. We could return early here and ask `main`
            // to re-run `read_sensors` for us, or run it automatically here.
            // Either way though, this would now trigger *extra* I2C traffic,
            // which is disappointing.
            //
            // Perhaps `reset_state` *shouldn't* clear `bsp.reset_all_values()`,
            // since the old code wouldn't have sent `NoData` to all of the
            // sensors?
            self.reset_state();
        }

        // `input` sensors have all been read during `read_sensors`.

        // The dynamic inputs don't depend on power mode; instead, they are
        // assumed to be present when a model exists in `self.dynamic_inputs`;
        // this model is set by external callers using
        // `register_dynamic_input` and `remove_dynamic_input`.
        //
        // TODO(AJM): Should we be doing this in `read_sensors` instead of
        // `run_control`?
        self.bsp.poll_dynamic_inputs(&self.sensor_api);

        // Run a common analysis pass first, regardless of state. Don't take any
        // side effectful actions yet though.
        let mut all_nominal = true;
        let mut any_power_down = None;
        let mut any_critical = None;
        let mut worst_margin = f32::MAX;
        let all_inputs_queried = self.bsp.all_inputs_queried();

        match self.state {
            ThermalControlState::Uncontrollable => {
                return Ok(ControlResult::PowerDown);
            }
            ThermalControlState::Boot => {
                // We allow boot to have not yet queried all items successfully
            }
            ThermalControlState::Running { .. }
            | ThermalControlState::Critical { .. }
            | ThermalControlState::FanParty => {
                // This should not be possible by construction, if we observe
                // this in the field we should investigate. For now, we will
                // conservatively return to the boot state and make a ringbuf
                // note.
                if !all_inputs_queried {
                    ringbuf_entry!(Trace::UnexpectedInputInactive);
                    self.reset_state();
                }
            }
        };

        self.bsp.all_active_inputs().for_each(
            |ActiveInputState {
                 sensor_id,
                 reading,
                 model,
             }| {
                let worst_case = reading.worst_case(now_ms, model);
                let temperature = worst_case.worst_case_temp;
                all_nominal &= model.is_nominal(temperature);
                if model.should_power_down(temperature) {
                    any_power_down = Some((sensor_id, worst_case));
                }
                if model.is_critical(temperature) {
                    any_critical = Some((sensor_id, worst_case));
                }

                // Remember, positive margin means that all parts are happily
                // below their max temperature; negative means someone is
                // overheating.  We want to pick the _smallest_ margin, since
                // that's the part which is most overheated.
                worst_margin = worst_margin.min(model.margin(temperature).0);
            },
        );

        //
        // Analysis is now complete. Begin performing control actions based on
        // that analysis.
        //

        // In any state, if we've reached the "any_power_down" threshold, then
        // it's time to go.
        if let Some(due_to) = any_power_down {
            return Ok(self.transition_to_uncontrollable_due_to(due_to, now_ms));
        }

        // TODO(AJM): I think we could dedupe some of the code below, basically
        // working backwards and checking if we "qualify" for each state, though
        // that's a bit more invasive of a change
        Ok(match &mut self.state {
            ThermalControlState::Boot => {
                if all_inputs_queried {
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
                } else if any_critical.is_none() {
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
        self.bsp.set_all_fan_duty(pwm)
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
        self.bsp.set_all_watchdogs(wd)
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

    pub fn register_dynamic_input(
        &mut self,
        index: usize,
        model: ThermalProperties,
    ) -> Result<(), ThermalError> {
        // If we're adding a new dynamic input, then reset the state to `Boot`,
        // ensuring that we'll wait for that channel to provide us with at least
        // one valid reading before resuming the PID loop.
        //
        // NOTE: We just ignore it if there was already a dynamic input there
        // already.
        let is_new = self.bsp.register_dynamic_input(index, model)?;
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

/// Decide what information to report about the given fan.
///
/// This includes updating:
///
/// - Sensor API data
/// - Ringbuf logging on state changes
/// - ereport logging on state changes
fn report_fan_state<D>(
    fan: &mut Fan<D>,
    sensor_api: &SensorApi,
    now_ms: u64,
    ereporter: &mut Ereporter,
) {
    // Make state matches a little less verbose
    use FanPresentState as Fps;
    use FanState as Fs;

    // Step one: report presence, if necessary
    let id = fan.rpm_sensor_id;
    if !fan.presence_acked {
        match fan.cur_state {
            Fs::NotPresent => {
                ringbuf_entry!(Trace::FanRemoved(id));
                _ = ereporter.deliver_ereport(&FanRemoved { id: id.into() });
            }
            Fs::Present(_) => {
                ringbuf_entry!(Trace::FanAdded(id));
                _ = ereporter.deliver_ereport(&FanInserted { id: id.into() })
            }
        };
        fan.presence_acked = true;
    }

    // Step two: report fan data to sensor API.
    let pres = match fan.cur_state {
        Fs::NotPresent => {
            // If the fan is physically not present, clear data immediately
            sensor_api.nodata(id, NoData::DeviceNotPresent, now_ms);
            // There's no more further state to log, return early
            fan.state_acked = true;
            return;
        }
        Fs::Present(pres) => pres,
    };
    match pres {
        // If the fan is unresponsive, clear the data from the sensor API
        Fps::Unresponsive(_) => {
            sensor_api.nodata(id, NoData::DeviceUnavailable, now_ms);
        }
        // If we have valid RPM data, report it immediately.
        Fps::Nominal(rpm) | Fps::TooFast(rpm) | Fps::TooSlow(rpm) => {
            sensor_api.post(id, rpm.0.into(), now_ms);
        }
    }

    // Step three: handle state reporting, if unreported
    if !fan.state_acked {
        let fan_info = || FanInfo {
            id: id.into(),
            lo_rpm_lim: fan.model.underspeed_rpm.0,
            hi_rpm_lim: fan.model.overspeed_rpm.0,
        };
        match pres {
            Fps::Unresponsive(e) => {
                _ = ereporter
                    .deliver_ereport(&FanRpmReadFailed { id: id.into() });
                ringbuf_entry!(Trace::FanReadFailed(id, e));
            }
            Fps::Nominal(_) => {
                _ = ereporter.deliver_ereport(&FanNominal { info: fan_info() });
                ringbuf_entry!(Trace::FanNominal(id));
            }
            Fps::TooFast(rpm) => {
                _ = ereporter.deliver_ereport(&FanOverspeed {
                    info: fan_info(),
                    rpm: rpm.0,
                });
                ringbuf_entry!(Trace::FanOverspeed(id, rpm));
            }
            Fps::TooSlow(rpm) => {
                _ = ereporter.deliver_ereport(&FanUnderspeed {
                    info: fan_info(),
                    rpm: rpm.0,
                });
                ringbuf_entry!(Trace::FanUnderspeed(id, rpm));
            }
        };
        fan.state_acked = true;
    }
}

ereports::declare_ereporter! {
    struct Ereporter<Ereport> {
        FanRemoved(FanRemoved),
        FanInserted(FanInserted),
        FanNominal(FanNominal),
        FanOverspeed(FanOverspeed),
        FanUnderspeed(FanUnderspeed),
        FanRpmReadFailed(FanRpmReadFailed),
    }
}

#[derive(Encode)]
struct FanInfo {
    id: u32,
    lo_rpm_lim: u16,
    hi_rpm_lim: u16,
}

/// An ereport representing a fan being removed
#[derive(Encode)]
#[ereport(class = "hw.remove.fan", version = 0)]
struct FanRemoved {
    id: u32,
}

/// An ereport representing a fan being inserted
#[derive(Encode)]
#[ereport(class = "hw.insert.fan", version = 0)]
struct FanInserted {
    id: u32,
}

/// An ereport representing a fan entering a nominal state
#[derive(Encode)]
#[ereport(class = "hw.fan.ok", version = 0)]
struct FanNominal {
    info: FanInfo,
}

/// An ereport representing a fan becoming overspeed
#[derive(Encode)]
#[ereport(class = "hw.fan.rpm.hi", version = 0)]
struct FanOverspeed {
    info: FanInfo,
    rpm: u16,
}

/// An ereport representing a fan becoming underspeed
#[derive(Encode)]
#[ereport(class = "hw.fan.rpm.lo", version = 0)]
struct FanUnderspeed {
    info: FanInfo,
    rpm: u16,
}

/// An ereport representing a failure to remove a fan
#[derive(Encode)]
#[ereport(class = "hw.fan.rpm.err", version = 0)]
struct FanRpmReadFailed {
    id: u32,
}
