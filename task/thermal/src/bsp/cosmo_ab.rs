// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for the Cosmo rev A hardware

use crate::{
    control::{
        ActiveInputState, ChannelType, InputPollingOutcome,
        MiscSensorPollingOutcome, PidConfig,
    },
    i2c_config::{devices, sensors},
};
pub use drv_cpu_seq_api::SeqError;
use drv_cpu_seq_api::{PowerState, Sequencer, StateChangeReason};
use drv_i2c_devices::max31790::I2cWatchdog;
use task_sensor_api::{Sensor, SensorId};
use task_thermal_api::{
    SANYO_DENKI_FAN_PROPERTIES, ThermalError, ThermalProperties,
};
use userlib::{
    TaskId, sys_get_timer, task_slot,
    units::{Celsius, PWMDuty},
};

// This BSP uses i2c temperature inputs
#[path = "./common/i2c_temp_input.rs"]
mod i2c_temp_input;
use i2c_temp_input::{
    Device, InputChannel, InputChannelMetadata, TemperatureSensor,
};

// This BSP uses the max31790 for fan control/monitoring
#[path = "./common/max31790.rs"]
mod max31790;
use max31790::Max31790State;

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

    /// Id of the I2C task, to query MAX5970 status
    i2c_task: TaskId,
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

impl crate::control::BspInterface for Bsp {
    /// This controller is tuned and ready to go
    const USE_CONTROLLER: bool = true;

    // Based on experimental tuning!
    const PID_CONFIG: PidConfig = PidConfig {
        zero: 35.0,
        gain_p: 5.0,
        gain_i: 0.0135,
        gain_d: 5.0,
        min_output: 0.0,
        max_output: 100.0,
    };

    type FanBspId = drv_i2c_devices::max31790::Fan;

    fn power_down(&self) -> Result<(), SeqError> {
        self.seq.set_state_with_reason(
            PowerState::A2,
            StateChangeReason::Overheat,
        )?;
        Ok(())
    }

    fn power_mode(&self) -> PowerBitmask {
        match self.seq.get_state() {
            PowerState::A0PlusHP => PowerBitmask::A0_PLUS_HP,
            PowerState::A0 | PowerState::A0Reset => PowerBitmask::A0,
            PowerState::A2
            | PowerState::A2PlusFans
            | PowerState::A0Thermtrip => PowerBitmask::A2,
        }
    }

    fn poll_fan_rpms(&mut self) -> impl Iterator<Item = &'_ mut Fan> {
        if let Ok(fctl) = self.fctrl.try_initialize() {
            let now = sys_get_timer().now;
            for fan in self.fans.iter_mut() {
                max31790::update_fan(
                    now,
                    true,
                    fctl,
                    fan,
                    &SANYO_DENKI_FAN_PROPERTIES,
                );
            }
        }

        self.fans.iter_mut()
    }

    fn poll_misc_sensors(
        &self,
    ) -> impl Iterator<Item = MiscSensorPollingOutcome> {
        self.misc_sensors.iter().map(|s| {
            let res = s.read_temp(self.i2c_task);
            MiscSensorPollingOutcome {
                sensor_id: s.sensor_id,
                outcome: res,
            }
        })
    }

    fn poll_inputs(
        &mut self,
        mode: PowerBitmask,
    ) -> impl Iterator<Item = InputPollingOutcome> {
        let task = &self.i2c_task;
        self.inputs
            .iter_mut()
            .map(move |i| i.poll_input(mode, task))
    }

    fn poll_dynamic_inputs(&mut self, _sensor_api: &Sensor) {
        // No dynamic inputs here
    }

    fn register_dynamic_input(
        &mut self,
        _index: usize,
        _model: ThermalProperties,
    ) -> Result<bool, ThermalError> {
        // No dynamic inputs here
        Err(ThermalError::InvalidIndex)
    }

    fn remove_dynamic_input(
        &mut self,
        _index: usize,
    ) -> Result<SensorId, ThermalError> {
        // No dynamic inputs here
        Err(ThermalError::InvalidIndex)
    }

    fn all_inputs_queried(&self) -> bool {
        self.inputs.iter().all(InputChannel::has_been_queried)
        // No dynamic inputs here
    }

    fn all_active_inputs(&self) -> impl Iterator<Item = ActiveInputState<'_>> {
        self.inputs.iter().filter_map(|input| input.active_state())
        // No dynamic inputs here
    }

    fn reset_all_values(&mut self) {
        let power = self.power_mode();
        self.inputs.iter_mut().for_each(|i| i.reset_value(power));
        // No dynamic inputs here
    }

    fn set_all_watchdogs(
        &mut self,
        watchdog: I2cWatchdog,
    ) -> Result<(), ThermalError> {
        // Only one watchdog to configure here!
        self.fctrl
            .try_initialize()?
            .set_watchdog(watchdog)
            .map_err(|_| ThermalError::DeviceError)
    }

    fn set_all_fan_duty(&mut self, duty: PWMDuty) -> Result<(), ThermalError> {
        let fctrl = self.fctrl.try_initialize()?;
        let mut any_err = false;

        // Note: DON'T short circuit here!
        for fan in self.fans.iter_mut() {
            any_err |= fctrl.set_pwm(fan.bsp_data, duty).is_err();
        }

        if any_err {
            Err(ThermalError::DeviceError)
        } else {
            Ok(())
        }
    }
}

impl Bsp {
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
            i2c_task,
            fctrl,

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
const FANS: [Fan; NUM_FANS] = max31790::make_consecutive_nonremovable_fans(
    &sensors::MAX31790_SPEED_SENSORS,
);

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
