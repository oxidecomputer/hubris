// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for the Cosmo rev A hardware

use crate::{
    control::{
        ChannelType, ControllerInitError, Device, FanControl, FanReading,
        InputChannel, InputChannelMetadata, InputStatus, Max31790State,
        PidConfig, TemperatureSensor,
    },
    i2c_config::{devices, sensors},
};
pub use drv_cpu_seq_api::SeqError;
use drv_cpu_seq_api::{PowerState, Sequencer, StateChangeReason};
use task_sensor_api::{Sensor, SensorId};
use task_thermal_api::{SensorReadError, ThermalError, ThermalProperties};
use userlib::{
    TaskId, UnwrapLite, sys_get_timer, task_slot,
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

    /// Fans
    fans: &'static mut [Fan; NUM_FANS],

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
    // fn fan_control(
    //     &mut self,
    //     fan: crate::Fan,
    // ) -> Result<FanControl<'_>, ControllerInitError> {
    //     let fctrl = self.fctrl.try_initialize()?;
    //     Ok(FanControl::Max31790(fctrl, fan.0.try_into().unwrap_lite()))
    // }

    // pub fn for_each_fctrl(
    //     &mut self,
    //     mut fctrl: impl FnMut(FanControl<'_>),
    // ) -> Result<(), ControllerInitError> {
    //     fctrl(self.fan_control(0.into())?);
    //     Ok(())
    // }

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

    pub fn read_fan_rpms(
        &mut self,
    ) -> Result<impl Iterator<Item = FanReading>, SeqError> {
        let mut fctrl =
            self.fctrl.try_initialize().map_err(SensorReadError::from);

        Ok(self.fans.iter_mut().map(move |f| {
            let was_present = f.last_reading.is_some();
            let res = fctrl.as_mut().map_err(|e| *e).and_then(|fc| {
                fc.fan_rpm(f.bsp_data).map_err(SensorReadError::I2cError)
            });
            match res {
                Ok(rpm) => {
                    f.last_reading = Some(rpm);
                    FanReading::PresentSuccess {
                        new: !was_present,
                        rpm,
                        sensor_id: f.rpm_sensor_id,
                        fan_id: f.bsp_data.into(),
                    }
                }
                Err(error) => {
                    f.last_reading = None;
                    FanReading::PresentError {
                        error,
                        sensor_id: f.rpm_sensor_id,
                        fan_id: f.bsp_data.into(),
                    }
                }
            }
        }))
    }

    pub fn misc_sensors(&self) -> impl Iterator<Item = &TemperatureSensor> {
        self.misc_sensors.iter()
    }

    pub fn inputs_mut(&mut self) -> impl Iterator<Item = &mut InputChannel> {
        self.inputs.iter_mut()
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
        self.inputs.iter().all(InputChannel::has_reading)
        // && self.dynamic_inputs...
    }

    // Visit all temperature sensors, first the inputs, then the dynamic_inputs.
    // Inputs and Dynamic Inputs that are missing will be skipped.
    pub fn all_inputs_allow_missing(
        &self,
    ) -> impl Iterator<Item = InputStatus<'_>> {
        self.inputs.iter().filter_map(InputChannel::status)
        // .zip(self.dynamic_inputs...)
    }

    // Visit all temperature sensors, first the inputs, then the dynamic_inputs.
    // All inputs MUST have a previous reading or this will panic, though the
    // readings may be allowed to be Missing if the model allows it. Dynamic
    // inputs that are not present will be skipped.
    pub fn all_inputs(&self) -> impl Iterator<Item = InputStatus<'_>> {
        self.inputs.iter().map(|input| input.status().unwrap_lite())
        // .zip(self.dynamic_inputs...)
    }

    pub fn reset_all_values(&mut self) {
        self.inputs.iter_mut().for_each(|i| i.reset_value());
        // self.dynamic_inputs...
    }

    // If a fan is missing, set PWMDuty(0). Attempt to apply to ALL fans,
    // even if some fail. return the LAST error if any.
    pub fn set_all_fan_rpms(
        &mut self,
        duty: PWMDuty,
    ) -> Result<(), ThermalError> {
        // let mut last_err = Ok(());
        // for idx in 0..NUM_FANS {
        //     let fctrl_res = self.fan_control(Fan::from(idx));
        //     let fctrl = match fctrl_res {
        //         Ok(f) => f,
        //         Err(e) => {
        //             last_err = Err(ThermalError::from(e));
        //             continue;
        //         }
        //     };

        //     if fctrl.set_pwm(duty).is_err() {
        //         last_err = Err(ThermalError::DeviceError);
        //     }
        // }

        // last_err
        todo!()
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

        static FANS_ONCE: static_cell::ClaimOnceCell<[Fan; NUM_FANS]> =
            static_cell::ClaimOnceCell::new(FANS);

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
            fans: FANS_ONCE.claim(),

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

// Our "bonus data" is a u8 that represents the fan's index in the i2c register
type Fan = crate::control::Fan<drv_i2c_devices::max31790::Fan>;
const FANS: [Fan; NUM_FANS] = [
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[0],
        drv_i2c_devices::max31790::Fan::new_const(0),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[1],
        drv_i2c_devices::max31790::Fan::new_const(1),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[2],
        drv_i2c_devices::max31790::Fan::new_const(2),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[3],
        drv_i2c_devices::max31790::Fan::new_const(3),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[4],
        drv_i2c_devices::max31790::Fan::new_const(4),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[5],
        drv_i2c_devices::max31790::Fan::new_const(5),
    ),
];

const INPUTS: [InputChannel; NUM_TEMPERATURE_INPUTS] = [
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::M2,
            devices::nvme_bmc_m2_a,
            sensors::NVME_BMC_M2_A_TEMPERATURE_SENSOR,
        ),
        M2_THERMALS,
        PowerBitmask::A0,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::M2,
            devices::nvme_bmc_m2_b,
            sensors::NVME_BMC_M2_B_TEMPERATURE_SENSOR,
        ),
        M2_THERMALS,
        PowerBitmask::A0,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::CPU,
            devices::sbtsi_cpu,
            sensors::SBTSI_CPU_TEMPERATURE_SENSOR,
        ),
        CPU_THERMALS,
        PowerBitmask::A0,
        ChannelType::MustBePresent,
    )),
    InputChannel::new(&InputChannelMetadata::new(
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
    )),
    // U.2 drives
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n0,
            sensors::NVME_BMC_U2_N0_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n1,
            sensors::NVME_BMC_U2_N1_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n2,
            sensors::NVME_BMC_U2_N2_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n3,
            sensors::NVME_BMC_U2_N3_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n4,
            sensors::NVME_BMC_U2_N4_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n5,
            sensors::NVME_BMC_U2_N5_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n6,
            sensors::NVME_BMC_U2_N6_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n7,
            sensors::NVME_BMC_U2_N7_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n8,
            sensors::NVME_BMC_U2_N8_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvme_bmc_u2_n9,
            sensors::NVME_BMC_U2_N9_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        PowerBitmask::A0,
        ChannelType::RemovableAndErrorProne,
    )),
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
