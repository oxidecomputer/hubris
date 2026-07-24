// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for the Gimlet rev B hardware

use crate::{
    control::{
        ChannelType, Device, FanPresence, FanReading, InputChannel,
        InputChannelMetadata, InputStatus, Max31790State, PidConfig,
        TemperatureSensor,
    },
    i2c_config::{devices, sensors},
};
pub use drv_cpu_seq_api::SeqError;
use drv_cpu_seq_api::{PowerState, Sequencer, StateChangeReason};
use drv_i2c_devices::max31790::I2cWatchdog;
use task_sensor_api::{Sensor, SensorId};
use task_thermal_api::{SensorReadError, ThermalError, ThermalProperties};
use userlib::{
    TaskId, UnwrapLite, task_slot,
    units::{Celsius, PWMDuty},
};

task_slot!(SEQ, gimlet_seq);

// We monitor the TMP117 air temperature sensors, but don't use them as part of
// the control loop.
const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS;

// BMC sensors vary depending on hardware revision
#[cfg(any(target_board = "gimlet-b", target_board = "gimlet-c"))]
const NUM_NVME_BMC_TEMPERATURE_SENSORS: usize =
    sensors::NUM_NVME_BMC_TEMPERATURE_SENSORS
        + sensors::NUM_M2_HP_ONLY_TEMPERATURE_SENSORS;

#[cfg(any(
    target_board = "gimlet-d",
    target_board = "gimlet-e",
    target_board = "gimlet-f"
))]
const NUM_NVME_BMC_TEMPERATURE_SENSORS: usize =
    sensors::NUM_NVME_BMC_TEMPERATURE_SENSORS;

// The control loop is driven by CPU, NIC, and DIMM temperatures
pub const NUM_TEMPERATURE_INPUTS: usize = sensors::NUM_SBTSI_TEMPERATURE_SENSORS
    + sensors::NUM_TMP451_TEMPERATURE_SENSORS
    + sensors::NUM_TSE2004AV_TEMPERATURE_SENSORS
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

    fans_added: bool,

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
        const A0_OR_A2 = Self::A0.bits() | Self::A2.bits();
        // Note that Gimlet does *not* need a separate flag for the A0+HP
        // power domain (like Cosmo does), as the T6 is powered in the A0
        // domain on Gimlet.

        // Bonus bits for M.2 power, which is switched separately.  We *cannot*
        // read the M.2 drives when they are unpowered; otherwise, we risk
        // locking up the I2C bus (see hardware-gimlet#1804 for the gory
        // details)
        const M2A = 0b00000100;
        const M2B = 0b00001000;
    }
}

impl crate::control::BspInterface for Bsp {
    // Based on experimental tuning!
    const PID_CONFIG: PidConfig = PidConfig {
        zero: 35.0,
        gain_p: 1.75,
        gain_i: 0.0135,
        gain_d: 0.4,
        min_output: 0.0,
        max_output: 100.0,
    };

    fn power_down(&self) -> Result<(), SeqError> {
        self.seq.set_state_with_reason(
            PowerState::A2,
            StateChangeReason::Overheat,
        )?;
        Ok(())
    }

    fn power_mode(&self) -> PowerBitmask {
        match self.seq.get_state() {
            PowerState::A0PlusHP | PowerState::A0 | PowerState::A0Reset => {
                use drv_i2c_devices::max5970;
                use userlib::units::Ohms;

                // The M.2 devices are enabled separately from A0, so we check
                // for them by asking their power controller. There's a
                // potential TOCTOU race here, but we don't expect to power
                // these down after the server comes up. (Note that we are
                // passing in the correct value of the current-sense resistor,
                // but we are in fact not using it here.)
                let dev = devices::max5970_m2(self.i2c_task);
                let m = max5970::Max5970::new(&dev, 0, Ohms(0.004), false);
                let mut out = PowerBitmask::A0;
                match m.read_reg(max5970::Register::status3) {
                    Ok(s) => {
                        // pg[0]
                        if s & (1 << 0) != 0 {
                            out |= PowerBitmask::M2A;
                        }
                        // pg[1]
                        if s & (1 << 1) != 0 {
                            out |= PowerBitmask::M2B;
                        }
                    }
                    // TODO: error handling here?
                    Err(_e) => (),
                }
                out
            }
            PowerState::A2
            | PowerState::A2PlusFans
            | PowerState::A0Thermtrip => PowerBitmask::A2,
        }
    }

    // We assume Gimlet fan presence cannot change
    fn read_fan_presence(
        &mut self,
    ) -> Result<impl Iterator<Item = FanPresence>, SeqError> {
        let report_new = !self.fans_added;
        self.fans_added = true;
        Ok(self.fans.iter().map(move |f| FanPresence::Present {
            fan_id: f.bsp_data.into(),
            new: report_new,
        }))
    }

    fn read_fan_rpms(&mut self) -> impl Iterator<Item = FanReading> {
        // Try to initialize the fan controller once at the start of the loop
        let mut fctrl =
            self.fctrl.try_initialize().map_err(SensorReadError::from);

        // TODO: Maybe there's a way to make this a method on Fan that we can
        // call, kind of like InputStatus?
        self.fans.iter_mut().map(move |f| {
            // If initialization failed, then we short circuit to return that
            // original error, copied for each fan we're going to report.
            let fctrl = fctrl.as_mut().map_err(|e| *e);

            // If it was a success, attempt to read the RPMs, and either report
            // that success or that error for each fan rpm.
            let res = fctrl.and_then(|fc| {
                fc.fan_rpm(f.bsp_data).map_err(SensorReadError::I2cError)
            });
            match res {
                Ok(rpm) => {
                    f.last_reading = Some(rpm);
                    FanReading::PresentSuccess {
                        rpm,
                        sensor_id: f.rpm_sensor_id,
                    }
                }
                Err(error) => {
                    f.last_reading = None;
                    FanReading::PresentError {
                        error,
                        sensor_id: f.rpm_sensor_id,
                    }
                }
            }
        })
    }

    fn misc_sensors(&self) -> impl Iterator<Item = &TemperatureSensor> {
        self.misc_sensors.iter()
    }

    fn inputs_mut(&mut self) -> impl Iterator<Item = &mut InputChannel> {
        self.inputs.iter_mut()
    }

    // TODO: This probably needs to exist, but for gimlet we have no dynamic
    // inputs to read back. This should read from the api and store the state
    fn read_dynamic_inputs_back_from_sensor_api(
        &mut self,
        _sensor_api: &Sensor,
    ) {
        // No dynamic inputs here
    }

    // returns Ok(true) if this was a new input
    fn update_dynamic_input(
        &mut self,
        _index: usize,
        _model: ThermalProperties,
    ) -> Result<bool, ThermalError> {
        // No dynamic inputs here, todo: static assert this
        Err(ThermalError::InvalidIndex)
    }

    // sets last_reading to Some(Missing), returns sensor id
    fn remove_dynamic_input(
        &mut self,
        _index: usize,
    ) -> Result<SensorId, ThermalError> {
        // No dynamic inputs here, todo: static assert this
        Err(ThermalError::InvalidIndex)
    }

    fn all_inputs_present(&self) -> bool {
        self.inputs.iter().all(InputChannel::has_reading)
        // && self.dynamic_inputs...
    }

    // Visit all temperature sensors, first the inputs, then the dynamic_inputs.
    // Inputs and Dynamic Inputs that are missing will be skipped.
    fn all_inputs_allow_missing(
        &self,
    ) -> impl Iterator<Item = InputStatus<'_>> {
        self.inputs.iter().filter_map(InputChannel::status)
        // .zip(self.dynamic_inputs...)
    }

    // Visit all temperature sensors, first the inputs, then the dynamic_inputs.
    // All inputs MUST have a previous reading or this will panic, though the
    // readings may be allowed to be Missing if the model allows it. Dynamic
    // inputs that are not present will be skipped.
    fn all_inputs(&self) -> impl Iterator<Item = InputStatus<'_>> {
        self.inputs.iter().map(|input| input.status().unwrap_lite())
        // .zip(self.dynamic_inputs...)
    }

    fn reset_all_values(&mut self) {
        self.inputs.iter_mut().for_each(|i| i.reset_value());
        // self.dynamic_inputs...
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

    // If a fan is missing, set PWMDuty(0). Attempt to apply to ALL fans,
    // even if some fail. return the LAST error if any.
    fn set_all_fan_rpms(&mut self, duty: PWMDuty) -> Result<(), ThermalError> {
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
            fans_added: false,

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
    // The M.2 devices are polled first deliberately: they're only polled if
    // powered, and we want to minimize the TOCTOU window between asking the
    // MAX5970 "is it powered?" and actually reading data.
    //
    // See hardware-gimlet#1804 for details; this is fixed in later revisions.
    InputChannel::new(&InputChannelMetadata::new(
        #[cfg(any(target_board = "gimlet-b", target_board = "gimlet-c"))]
        TemperatureSensor::new(
            Device::M2,
            devices::m2_hp_only_m2_a,
            sensors::M2_HP_ONLY_M2_A_TEMPERATURE_SENSOR,
        ),
        #[cfg(any(
            target_board = "gimlet-d",
            target_board = "gimlet-e",
            target_board = "gimlet-f"
        ))]
        TemperatureSensor::new(
            Device::M2,
            devices::nvme_bmc_m2_a,
            sensors::NVME_BMC_M2_A_TEMPERATURE_SENSOR,
        ),
        M2_THERMALS,
        PowerBitmask::M2A,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        #[cfg(any(target_board = "gimlet-b", target_board = "gimlet-c"))]
        TemperatureSensor::new(
            Device::M2,
            devices::m2_hp_only_m2_b,
            sensors::M2_HP_ONLY_M2_B_TEMPERATURE_SENSOR,
        ),
        #[cfg(any(
            target_board = "gimlet-d",
            target_board = "gimlet-e",
            target_board = "gimlet-f"
        ))]
        TemperatureSensor::new(
            Device::M2,
            devices::nvme_bmc_m2_b,
            sensors::NVME_BMC_M2_B_TEMPERATURE_SENSOR,
        ),
        M2_THERMALS,
        PowerBitmask::M2B,
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
        PowerBitmask::A0,
        ChannelType::MustBePresent,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_a0,
            sensors::TSE2004AV_DIMM_A0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_a1,
            sensors::TSE2004AV_DIMM_A1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_b0,
            sensors::TSE2004AV_DIMM_B0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_b1,
            sensors::TSE2004AV_DIMM_B1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_c0,
            sensors::TSE2004AV_DIMM_C0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_c1,
            sensors::TSE2004AV_DIMM_C1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_d0,
            sensors::TSE2004AV_DIMM_D0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_d1,
            sensors::TSE2004AV_DIMM_D1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_e0,
            sensors::TSE2004AV_DIMM_E0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_e1,
            sensors::TSE2004AV_DIMM_E1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_f0,
            sensors::TSE2004AV_DIMM_F0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_f1,
            sensors::TSE2004AV_DIMM_F1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_g0,
            sensors::TSE2004AV_DIMM_G0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_g1,
            sensors::TSE2004AV_DIMM_G1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_h0,
            sensors::TSE2004AV_DIMM_H0_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Dimm,
            devices::tse2004av_dimm_h1,
            sensors::TSE2004AV_DIMM_H1_TEMPERATURE_SENSOR,
        ),
        DIMM_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::Removable,
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
