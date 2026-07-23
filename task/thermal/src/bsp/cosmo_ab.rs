// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for the Cosmo rev A hardware

use crate::{
    Fan,
    control::{
        ChannelType, ControllerInitError, Device, FanControl, InputChannel,
        Max31790State, PidConfig, TemperatureReading, TemperatureSensor,
        TimestampedTemperatureReading, unexpected_failure,
    },
    i2c_config::{devices, sensors},
};
pub use drv_cpu_seq_api::SeqError;
use drv_cpu_seq_api::{PowerState, Sequencer, StateChangeReason};
use task_sensor_api::{NoData, Sensor, SensorError, SensorId};
use task_thermal_api::{SensorReadError, ThermalError, ThermalProperties};
use userlib::{
    TaskId, UnwrapLite, task_slot,
    units::{Celsius, PWMDuty},
};

task_slot!(SEQ, cosmo_seq);

// We monitor the TMP117 air temperature sensors, but don't use them as part of
// the control loop.
const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS;

const NUM_NVME_BMC_TEMPERATURE_SENSORS: usize =
    sensors::NUM_NVME_BMC_TEMPERATURE_SENSORS;

// The control loop is driven by CPU, NIC, and BMC temperatures
// XXX we should also monitor DIMM temperatures here
const NUM_TEMPERATURE_INPUTS: usize = sensors::NUM_SBTSI_TEMPERATURE_SENSORS
    + sensors::NUM_TMP451_TEMPERATURE_SENSORS
    + NUM_NVME_BMC_TEMPERATURE_SENSORS;

// We've got 6 fans, driven from a single MAX31790 IC
const NUM_FANS: usize = drv_i2c_devices::max31790::MAX_FANS as usize;

/// This controller is tuned and ready to go
pub const USE_CONTROLLER: bool = true;

pub(crate) struct Bsp {
    /// Controlled sensors
    inputs: &'static mut [InputChannel; NUM_TEMPERATURE_INPUTS],

    /// Monitored sensors
    misc_sensors: &'static [TemperatureSensor; NUM_TEMPERATURE_SENSORS],

    /// Fan control IC
    fctrl: Max31790State,

    /// Handle to the sequencer task, to query power state
    seq: Sequencer,

    /// Tuning for the PID controller
    pub pid_config: PidConfig,
}

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct PowerBitmask: u32 {
        // As far as I know, we don't have any devices which are active only
        // in A2; you probably want to use `A0_OR_A2` instead.
        const A2 = 0b00000001;
        const A0 = 0b00000010;
        // A0+HP: T6 power is enabled by the host processor, in addition to
        // all A0 devices.
        const T6 = 0b00000100;
        const A0_PLUS_HP = Self::A0.bits() | Self::T6.bits();
    }
}

impl Bsp {
    pub fn fan_control(
        &mut self,
        fan: crate::Fan,
    ) -> Result<FanControl<'_>, ControllerInitError> {
        let fctrl = self.fctrl.try_initialize()?;
        Ok(FanControl::Max31790(fctrl, fan.0.try_into().unwrap_lite()))
    }

    pub fn for_each_fctrl(
        &mut self,
        mut fctrl: impl FnMut(FanControl<'_>),
    ) -> Result<(), ControllerInitError> {
        fctrl(self.fan_control(0.into())?);
        Ok(())
    }

    pub fn power_down(&self) -> Result<(), SeqError> {
        self.seq.set_state_with_reason(
            PowerState::A2,
            StateChangeReason::Overheat,
        )?;
        Ok(())
    }

    pub fn power_mode(&self) -> PowerBitmask {
        match self.seq.get_state() {
            PowerState::A0PlusHP => PowerBitmask::A0_PLUS_HP,
            PowerState::A0 | PowerState::A0Reset => PowerBitmask::A0,
            PowerState::A2
            | PowerState::A2PlusFans
            | PowerState::A0Thermtrip => PowerBitmask::A2,
        }
    }

    pub fn update_fan_presence<F, G>(
        &mut self,
        _on_added: F,
        _on_remove: G,
    ) -> Result<(), SeqError>
    where
        F: Fn(&Fan),
        G: Fn(&Fan),
    {
        // Our fans are always here, never added or removed!
        Ok(())
    }

    pub fn read_fan_rpms(
        &mut self,
        mut on_success: impl FnMut(&SensorId, f32) -> u64,
        mut on_error: impl FnMut(&SensorId, SensorReadError) -> u64,
        _on_missing: impl FnMut(&SensorId) -> u64,
    ) {
        for (idx, sensor) in sensors::MAX31790_SPEED_SENSORS.iter().enumerate()
        {
            // TODO: Why does this use idx?
            let fctrl_res = self.fan_control(Fan::from(idx));
            let fctrl = match fctrl_res {
                Ok(f) => f,
                Err(e) => {
                    on_error(sensor, SensorReadError::from(e));
                    continue;
                }
            };

            // TODO(AJM): Keep last fan RPM?
            match fctrl.fan_rpm() {
                Ok(reading) => on_success(sensor, reading.0.into()),
                Err(e) => on_error(sensor, SensorReadError::I2cError(e)),
            };
        }
    }

    pub fn read_misc_sensors(
        &mut self,
        i2c_task: TaskId,
        mut on_success: impl FnMut(&SensorId, f32) -> u64,
        mut on_error: impl FnMut(&SensorId, SensorReadError) -> u64,
    ) {
        for s in self.misc_sensors.iter() {
            match s.read_temp(i2c_task) {
                Ok(v) => on_success(&s.sensor_id, v.0),
                Err(e) => on_error(&s.sensor_id, e),
            };
        }
    }

    pub fn read_inputs(
        &mut self,
        mode: PowerBitmask,
        i2c_task: TaskId,
        mut on_success: impl FnMut(&SensorId, f32) -> u64,
        mut on_unexp_error: impl FnMut(&InputChannel, SensorReadError) -> u64,
        mut on_error: impl FnMut(&InputChannel, SensorReadError) -> u64,
        mut on_unpowered: impl FnMut(&SensorId) -> u64,
    ) {
        // NOTE(AJM): This combines what used to be two passes before with
        // `read_sensors` and `run_control`. Previously, the former would read
        // all of the sensors and post them to the sensor task, and the latter
        // would read them back and store them as state. Now, I do that all in
        // just the first pass during `read_sensors`. This has *some* side
        // effect, as before we wouldn't retain state unless we were running
        // control, but now we always do. HOWEVER, this is not observable state,
        // as Manual mode does nothing with this persistence, and whenever we
        // enter the Auto state (which does control), we purge all the old
        // values anyway!
        for s in self.inputs.iter_mut() {
            if !mode.intersects(s.power_mode_mask) {
                let _now = on_unpowered(&s.sensor.sensor_id);
                s.last_reading = Some(TemperatureReading::Inactive);
                continue;
            }

            match s.sensor.read_temp(i2c_task) {
                Ok(v) => {
                    let now = on_success(&s.sensor.sensor_id, v.0);
                    s.last_reading = Some(TemperatureReading::Valid(
                        TimestampedTemperatureReading {
                            time_ms: now,
                            value: v,
                        },
                    ));
                }
                Err(e) => {
                    // The current `unexpected_failure` comes from
                    // `read_sensors`, which is only deciding whether it's worth
                    // logging about. In either case, it will push NoData to the
                    // sensor api.
                    let e1 = e.clone();
                    if unexpected_failure(s, e) {
                        let _now = on_unexp_error(s, e);
                    } else {
                        let _now = on_error(s, e);
                    }

                    // However, when we later would have stored the state value
                    // for persistence in `run_control`, that used slightly
                    // different logic, ONLY clearing the persisted value if:
                    //
                    // - The sensor is not present AND removable
                    // - The sensor is error prone
                    //
                    // Replicate that logic here, doing some type shenanigans
                    // because we aren't round-tripping through the Sensor API
                    // anymore.
                    let e2 = NoData::from(e1);
                    let e3 = SensorError::from(e2);
                    match (s.ty, e3) {
                        (ChannelType::Removable, SensorError::NotPresent) => {
                            s.last_reading = None;
                        }
                        (ChannelType::RemovableAndErrorProne, _) => {
                            s.last_reading = None;
                        }
                        _ => {
                            // In all other cases, just leave whatever the last
                            // present value was so that the state estimation
                            // can continue estimating state.
                        }
                    }
                }
            };
        }
    }

    // TODO: This probably needs to exist, but for cosmo we have no dynamic
    // inputs to read back. This should read from the api and store the state
    pub fn read_dynamic_inputs_back_from_sensor_api(
        &mut self,
        _sensor_api: &Sensor,
    ) {
        // No dynamic inputs here
    }

    // returns Ok(true) if this was a new input
    pub fn update_dynamic_input(
        &mut self,
        _index: usize,
        _model: ThermalProperties,
    ) -> Result<bool, ThermalError> {
        // No dynamic inputs here, todo: static assert this
        Err(ThermalError::InvalidIndex)
    }

    // sets last_reading to Some(Missing), returns sensor id
    pub fn remove_dynamic_input(
        &mut self,
        _index: usize,
    ) -> Result<SensorId, ThermalError> {
        // No dynamic inputs here, todo: static assert this
        Err(ThermalError::InvalidIndex)
    }

    pub fn all_inputs_present(&self) -> bool {
        self.inputs.iter().all(|i| i.last_reading.is_some())
        // && self.dynamic_inputs...
    }

    // Visit all temperature sensors, first the inputs, then the dynamic_inputs.
    // Inputs and Dynamic Inputs that are missing will be skipped.
    pub fn for_each_temp_allow_missing_inputs(
        &self,
        mut f: impl FnMut(SensorId, TemperatureReading, ThermalProperties),
    ) {
        let iter = self.inputs.iter().filter_map(|input| {
            let last = input.last_reading?;
            Some((input.sensor.sensor_id, last, input.model))
        });
        for (sensor_id, reading, model) in iter {
            f(sensor_id, reading, model);
        }

        // for _dinput in self.dynamic_inputs...
    }

    // Visit all temperature sensors, first the inputs, then the dynamic_inputs.
    // All inputs MUST have a previous reading or this will panic, though the
    // readings may be allowed to be Missing if the model allows it. Dynamic
    // inputs that are not present will be skipped.
    pub fn for_each_temp(
        &self,
        mut f: impl FnMut(SensorId, TemperatureReading, ThermalProperties),
    ) {
        for input in self.inputs.iter() {
            let reading = input.last_reading.unwrap_lite();
            f(input.sensor.sensor_id, reading, input.model);
        }

        // for _dinput in self.dynamic_inputs...
    }

    pub fn reset_all_values(&mut self) {
        self.inputs.iter_mut().for_each(|i| i.last_reading = None);
        // self.dynamic_inputs...
    }

    // If a fan is missing, set PWMDuty(0). Attempt to apply to ALL fans,
    // even if some fail. return the LAST error if any.
    pub fn set_all_fan_rpms(
        &mut self,
        duty: PWMDuty,
    ) -> Result<(), ThermalError> {
        let mut last_err = Ok(());
        for idx in 0..NUM_FANS {
            let fctrl_res = self.fan_control(Fan::from(idx));
            let fctrl = match fctrl_res {
                Ok(f) => f,
                Err(e) => {
                    last_err = Err(ThermalError::from(e));
                    continue;
                }
            };

            if fctrl.set_pwm(duty).is_err() {
                last_err = Err(ThermalError::DeviceError);
            }
        }

        last_err
    }

    // pub fn fan_sensor_id(&self, i: usize) -> SensorId {
    //     sensors::MAX31790_SPEED_SENSORS[i]
    // }

    pub fn new(i2c_task: TaskId) -> Self {
        // Initializes and build a handle to the fan controller IC
        let fctrl = Max31790State::new(&devices::max31790(i2c_task)[0]);

        // Handle for the sequencer task, which we check for power state
        let seq = Sequencer::from(SEQ.get_task_id());
        static INPUTS_ONCE: static_cell::ClaimOnceCell<
            [InputChannel; NUM_TEMPERATURE_INPUTS],
        > = static_cell::ClaimOnceCell::new(INPUTS);

        Self {
            seq,
            fctrl,

            // Based on experimental tuning!
            pid_config: PidConfig {
                zero: 35.0,
                gain_p: 5.0,
                gain_i: 0.0135,
                gain_d: 5.0,
                min_output: 0.0,
                max_output: 100.0,
            },

            inputs: INPUTS_ONCE.claim(),

            // We monitor and log all of the air temperatures
            misc_sensors: &MISC_SENSORS,
        }
    }
}

// In general, see RFD 276 Detailed Thermal Loop Design for references.
// TODO: temperature_slew_deg_per_sec is made up.

// Thermal throttling begins at 78° for WD-SN840 (primary source) and
// 75° for Micron-9300 (secondary source).
//
// For the WD part, thermal shutdown is at 84°C, which also voids the
// warranty. The Micron drive doesn't specify a thermal shutdown
// temperature, but the "critical" temperature is 80°C.
//
// All temperature are "composite" temperatures.
const U2_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(65f32),
    critical_temperature: Celsius(70f32),
    power_down_temperature: Celsius(75f32),
    temperature_slew_deg_per_sec: 0.5,
};

// The Micron-7300 (primary source) begins throttling at 72°, and its "critical
// composite temperature" is 76°.  The WD-SN640 (secondary source) begins
// throttling at 77°C.
const M2_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(65f32),
    critical_temperature: Celsius(70f32),
    power_down_temperature: Celsius(75f32),
    temperature_slew_deg_per_sec: 0.5,
};

// The CPU doesn't actually report true temperature; it reports a
// unitless "temperature control value".  Throttling starts at 95, and
// becomes more aggressive at 100.  Let's aim for 80, to stay well below
// the throttling range.
const CPU_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(80f32),
    critical_temperature: Celsius(90f32),
    power_down_temperature: Celsius(100f32),
    temperature_slew_deg_per_sec: 0.5,
};

// According to Chelsio, T_j Max is 115°C, while T_j Typical is 100° C. Let's
// try to stay below 100°C.
const T6_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(95f32),
    critical_temperature: Celsius(100f32),
    power_down_temperature: Celsius(115f32),
    temperature_slew_deg_per_sec: 0.5,
};

const INPUTS: [InputChannel; NUM_TEMPERATURE_INPUTS] = [
    InputChannel::new(
        TemperatureSensor::new(
            Device::M2,
            devices::nvme_bmc_m2_a,
            sensors::NVME_BMC_M2_A_TEMPERATURE_SENSOR,
        ),
        M2_THERMALS,
        PowerBitmask::A0,
        ChannelType::Removable,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::M2,
            devices::nvme_bmc_m2_b,
            sensors::NVME_BMC_M2_B_TEMPERATURE_SENSOR,
        ),
        M2_THERMALS,
        PowerBitmask::A0,
        ChannelType::Removable,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::CPU,
            devices::sbtsi_cpu,
            sensors::SBTSI_CPU_TEMPERATURE_SENSOR,
        ),
        CPU_THERMALS,
        PowerBitmask::A0,
        ChannelType::MustBePresent,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Tmp451(drv_i2c_devices::tmp451::Target::Remote),
            devices::tmp451_t6,
            sensors::TMP451_T6_TEMPERATURE_SENSOR,
        ),
        T6_THERMALS,
        // Enabled only if we are in the A0+HP power state, as T6 power is
        // controlled by the host OS.
        PowerBitmask::T6,
        ChannelType::MustBePresent,
    ),
    // U.2 drives
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n0,
            sensors::NVME_BMC_U2_N0_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n1,
            sensors::NVME_BMC_U2_N1_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n2,
            sensors::NVME_BMC_U2_N2_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n3,
            sensors::NVME_BMC_U2_N3_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n4,
            sensors::NVME_BMC_U2_N4_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n5,
            sensors::NVME_BMC_U2_N5_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n6,
            sensors::NVME_BMC_U2_N6_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n7,
            sensors::NVME_BMC_U2_N7_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n8,
            sensors::NVME_BMC_U2_N8_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n9,
            sensors::NVME_BMC_U2_N9_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    ),
];

const MISC_SENSORS: [TemperatureSensor; NUM_TEMPERATURE_SENSORS] = [
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_southwest,
        sensors::TMP117_SOUTHWEST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_southeast,
        sensors::TMP117_SOUTHEAST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_northwest,
        sensors::TMP117_NORTHWEST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_northeast,
        sensors::TMP117_NORTHEAST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_north,
        sensors::TMP117_NORTH_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_south,
        sensors::TMP117_SOUTH_TEMPERATURE_SENSOR,
    ),
];
