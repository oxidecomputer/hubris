// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for the Gimlet rev B hardware

use crate::{
    control::{
        Device, FanControl, InputChannel, PidConfig, TemperatureSensor,
        ThermalProperties,
    },
    i2c_config::{devices, sensors},
};
use core::convert::TryInto;
pub use drv_gimlet_seq_api::SeqError;
use drv_gimlet_seq_api::{PowerState, Sequencer};
use drv_i2c_devices::max31790::Max31790;
use task_sensor_api::SensorId;
use userlib::{task_slot, units::Celsius, TaskId};

task_slot!(SEQ, gimlet_seq);

// We monitor the TMP117 air temperature sensors, but don't use them as part of
// the control loop.
const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS;

// The control loop is driven by CPU, NIC, and DIMM temperatures
pub const NUM_TEMPERATURE_INPUTS: usize = sensors::NUM_SBTSI_TEMPERATURE_SENSORS
    + sensors::NUM_TMP451_TEMPERATURE_SENSORS
    + sensors::NUM_TSE2004AV_TEMPERATURE_SENSORS
    + sensors::NUM_NVMEBMC_TEMPERATURE_SENSORS;

// We've got 6 fans, driven from a single MAX31790 IC
const NUM_FANS: usize = drv_i2c_devices::max31790::MAX_FANS as usize;

/// This controller is tuned and ready to go
pub const USE_CONTROLLER: bool = true;

pub(crate) struct Bsp {
    /// Controlled sensors
    pub inputs: &'static [InputChannel],

    /// Monitored sensors
    pub misc_sensors: &'static [TemperatureSensor],

    /// Fan RPM sensors
    pub fans: [SensorId; NUM_FANS],

    /// Fan control IC
    fctrl: Max31790,

    /// Handle to the sequencer task, to query power state
    seq: Sequencer,

    /// Tuning for the PID controller
    pub pid_config: PidConfig,
}

// Use bitmasks to determine when sensors should be polled
const POWER_STATE_A2: u32 = 0b001;
const POWER_STATE_A0: u32 = 0b010;

impl Bsp {
    pub fn fan_control(&self, fan: crate::Fan) -> FanControl<'_> {
        FanControl::Max31790(&self.fctrl, fan.0.try_into().unwrap())
    }

    pub fn for_each_fctrl(&self, mut fctrl: impl FnMut(FanControl<'_>)) {
        fctrl(self.fan_control(0.into()))
    }

    pub fn power_down(&self) -> Result<(), SeqError> {
        self.seq.set_state(PowerState::A2)
    }

    pub fn power_mode(&self) -> u32 {
        match self.seq.get_state() {
            Ok(p) => match p {
                PowerState::A0PlusHP | PowerState::A0 | PowerState::A1 => {
                    POWER_STATE_A0
                }
                PowerState::A2
                | PowerState::A2PlusMono
                | PowerState::A2PlusFans
                | PowerState::A0Thermtrip => POWER_STATE_A2,
            },
            // If `get_state` failed, then enable all sensors.  One of them
            // will presumably fail and will drop us into failsafe
            Err(_) => u32::MAX,
        }
    }

    pub fn new(i2c_task: TaskId) -> Self {
        // Awkwardly build the fan array, because there's not a great way
        // to build a fixed-size array from a function
        let mut fans = [None; NUM_FANS];
        for (i, f) in fans.iter_mut().enumerate() {
            *f = Some(sensors::MAX31790_SPEED_SENSORS[i]);
        }
        let fans = fans.map(Option::unwrap);

        // Initializes and build a handle to the fan controller IC
        let fctrl = Max31790::new(&devices::max31790(i2c_task)[0]);
        fctrl.initialize().unwrap();

        // Handle for the sequencer task, which we check for power state
        let seq = Sequencer::from(SEQ.get_task_id());

        Self {
            seq,
            fans,
            fctrl,

            // Based on experimental tuning!
            pid_config: PidConfig {
                // If we're > 10 degrees from the target temperature, fans
                // should be on at full power.
                gain_p: 10.0,
                gain_i: 0.5,
                gain_d: 10.0,
            },

            inputs: &INPUTS,

            // We monitor and log all of the air temperatures
            misc_sensors: &MISC_SENSORS,
        }
    }
}

// In general, see RFD 276 Detailed Thermal Loop Design for references.
// TODO: temperature_slew_deg_per_sec is made up.

// JEDEC specification requires Tcasemax <= 85°C for normal temperature
// range.  We're using RAM with industrial temperature ranges, listed on
// the datasheet as 0°C <= T_oper <= 95°C.
const DIMM_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(80f32),
    critical_temperature: Celsius(90f32),
    power_down_temperature: Celsius(95f32),
    temperature_slew_deg_per_sec: 0.5,
};

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

// The T6's specifications aren't clearly detailed anywhere.
const T6_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(70f32),
    critical_temperature: Celsius(80f32),
    power_down_temperature: Celsius(85f32),
    temperature_slew_deg_per_sec: 0.5,
};

const INPUTS: [InputChannel; NUM_TEMPERATURE_INPUTS] = [
    InputChannel::new(
        TemperatureSensor::new(
            Device::CPU,
            devices::sbtsi_cpu,
            sensors::SBTSI_CPU_TEMPERATURE_SENSOR,
        ),
        CPU_THERMALS,
        POWER_STATE_A0,
        false,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Tmp451(drv_i2c_devices::tmp451::Target::Remote),
            devices::tmp451_t6,
            sensors::TMP451_T6_TEMPERATURE_SENSOR,
        ),
        T6_THERMALS,
        POWER_STATE_A0,
        false,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_a0,
            sensors::TSE2004AV_DIMM_A0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_a1,
            sensors::TSE2004AV_DIMM_A1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_b0,
            sensors::TSE2004AV_DIMM_B0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_b1,
            sensors::TSE2004AV_DIMM_B1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_c0,
            sensors::TSE2004AV_DIMM_C0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_c1,
            sensors::TSE2004AV_DIMM_C1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_d0,
            sensors::TSE2004AV_DIMM_D0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_d1,
            sensors::TSE2004AV_DIMM_D1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_e0,
            sensors::TSE2004AV_DIMM_E0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_e1,
            sensors::TSE2004AV_DIMM_E1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_f0,
            sensors::TSE2004AV_DIMM_F0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_f1,
            sensors::TSE2004AV_DIMM_F1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_g0,
            sensors::TSE2004AV_DIMM_G0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_g1,
            sensors::TSE2004AV_DIMM_G1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_h0,
            sensors::TSE2004AV_DIMM_H0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_h1,
            sensors::TSE2004AV_DIMM_H1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        POWER_STATE_A0 | POWER_STATE_A2,
        true,
    ),
    // U.2 drives
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n0,
            sensors::NVMEBMC_U2_N0_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n1,
            sensors::NVMEBMC_U2_N1_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n2,
            sensors::NVMEBMC_U2_N2_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n3,
            sensors::NVMEBMC_U2_N3_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n4,
            sensors::NVMEBMC_U2_N4_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n5,
            sensors::NVMEBMC_U2_N5_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n6,
            sensors::NVMEBMC_U2_N6_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n7,
            sensors::NVMEBMC_U2_N7_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n8,
            sensors::NVMEBMC_U2_N8_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
    ),
    InputChannel::new(
        TemperatureSensor::new(
            Device::U2,
            devices::nvmebmc_u2_n9,
            sensors::NVMEBMC_U2_N9_TEMPERATURE_SENSOR,
        ),
        U2_THERMALS,
        POWER_STATE_A0,
        true,
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
