// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! BSP for Sidecar

use crate::control::{
    ChannelType, DynamicTemperatureState, FanPresence, InputStatus, PidConfig,
    TimestampedTemperatureReading,
};
use crate::control::{DynamicInputChannel, FanStatus};
use drv_i2c_devices::max31790::Max31790;
use drv_i2c_devices::tmp451::*;
pub use drv_sidecar_seq_api::SeqError;
use drv_sidecar_seq_api::{Sequencer, TofinoSeqState, TofinoSequencerPolicy};
use task_sensor_api::SensorId;
use task_thermal_api::SensorReadError;
use task_thermal_api::ThermalError;
use task_thermal_api::ThermalProperties;
use userlib::{TaskId, task_slot, units::Celsius};

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;
use i2c_config::sensors;

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

task_slot!(SEQUENCER, sequencer);

////////////////////////////////////////////////////////////////////////////////
// Constants!

// Air temperature sensors, which aren't used in the control loop
const NUM_TEMPERATURE_SENSORS: usize = sensors::NUM_TMP117_TEMPERATURE_SENSORS;

// Temperature inputs (I2C devices), which are used in the control loop.
pub const NUM_TEMPERATURE_INPUTS: usize =
    sensors::NUM_TMP451_TEMPERATURE_SENSORS;

// External temperature inputs, which are provided to the task over IPC
// In practice, these are our transceivers.
pub const NUM_DYNAMIC_TEMPERATURE_INPUTS: usize =
    drv_transceivers_api::NUM_PORTS as usize;

// Number of individual fans
pub const NUM_FANS: usize = sensors::NUM_MAX31790_SPEED_SENSORS;

////////////////////////////////////////////////////////////////////////////////

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct PowerBitmask: u32 {
        // As far as I know, we don't have any devices which are active only
        // in A2; you probably want to use `POWER_STATE_A0_OR_A2` instead
        const A2 = 0b00000001;
        const A0 = 0b00000010;
        const A0_OR_A2 = Self::A0.bits() | Self::A2.bits();
    }
}

#[allow(dead_code)]
pub(crate) struct Bsp {
    /// Controlled sensors
    pub inputs: &'static mut [InputChannel; NUM_TEMPERATURE_INPUTS],
    pub dynamic_inputs:
        &'static mut [DynamicInputChannel; NUM_DYNAMIC_TEMPERATURE_INPUTS],

    /// Monitored sensors
    pub misc_sensors: &'static [TemperatureSensor; NUM_TEMPERATURE_SENSORS],

    /// Our two fan controllers: east for 0/1 and west for 1/2
    fctrl_east: Max31790State,
    fctrl_west: Max31790State,
    fans: &'static mut [Fan; NUM_FANS],

    seq: Sequencer,
    i2c_task: TaskId,

    pub pid_config: PidConfig,
}

impl crate::control::BspInterface for Bsp {
    // Run the PID loop on startup
    const USE_CONTROLLER: bool = true;

    // TODO: this is all made up, copied from tuned Gimlet values
    const PID_CONFIG: PidConfig = PidConfig {
        zero: 35.0,
        gain_p: 1.75,
        gain_i: 0.0135,
        gain_d: 0.4,
        min_output: 0.0,
        max_output: 100.0,
    };

    fn power_down(&self) -> Result<(), crate::SeqError> {
        self.seq
            .set_tofino_seq_policy(TofinoSequencerPolicy::Disabled)
    }

    fn power_mode(&self) -> PowerBitmask {
        match self.seq.tofino_seq_state() {
            Ok(r) => match r {
                TofinoSeqState::A0 => PowerBitmask::A0,
                TofinoSeqState::Init
                | TofinoSeqState::A2
                | TofinoSeqState::InPowerUp
                | TofinoSeqState::InPowerDown => PowerBitmask::A2,
            },
            Err(_) => PowerBitmask::A0_OR_A2,
        }
    }

    fn read_fan_presence(
        &mut self,
    ) -> Result<impl Iterator<Item = FanPresence>, crate::SeqError> {
        // Get presence bits from the sequencer
        let iter = self
            .seq
            .fan_module_presence()?
            // First, get an iterator over all of the presence bools.
            .0
            .into_iter()
            // Since each bool represets the state of two fans at a time, we
            // chunk up the fans in pairs, and dupe the presence bit onto each
            // one, THEN flatten it back into a single linear iterator.
            .zip(self.fans.chunks_exact_mut(2))
            .flat_map(|(p, c)| core::iter::repeat(p).zip(c.iter_mut()))
            // Finally, for each fan, see if it is newly here/gone, and report
            // that with its "fan" ID, which is just the order that we define
            // our fans. We don't change the order, so it's okay to enumerate
            // "late" instead of earlier in the chain.
            .enumerate()
            .map(|(fan_id, (present, c))| {
                let fan_id = fan_id as u8;
                let was = c.is_present;
                let new = was ^ present;
                c.is_present = present;

                if present {
                    FanPresence::Present { fan_id, new }
                } else {
                    FanPresence::NotPresent { fan_id, new }
                }
            });
        Ok(iter)
    }

    fn read_fan_rpms(&mut self) -> impl Iterator<Item = FanStatus> {
        // Load bearing assumption: the first 4 fans are the EAST fans, and the
        // last 4 fans are the WEST fans.
        let (east, west) = self.fans.split_at_mut(4);
        self.fctrl_east
            .read_fan_rpms(east)
            .chain(self.fctrl_west.read_fan_rpms(west))
    }

    fn read_misc_sensors(
        &self,
    ) -> impl Iterator<Item = (SensorId, Result<Celsius, SensorReadError>)>
    {
        self.misc_sensors.iter().map(|s| {
            let res = s.read_temp(self.i2c_task);
            (s.sensor_id, res)
        })
    }

    fn read_inputs(
        &mut self,
        mode: PowerBitmask,
    ) -> impl Iterator<Item = crate::control::InputReadingOutcome> {
        let task = &self.i2c_task;
        self.inputs
            .iter_mut()
            .map(move |i| i.do_reading(mode, task))
    }

    fn read_dynamic_inputs_back_from_sensor_api(
        &mut self,
        sensor_api: &task_sensor_api::Sensor,
    ) {
        for di in self.dynamic_inputs.iter_mut() {
            // If the input is disabled, don't attempt to read.
            let model = match di.state {
                DynamicTemperatureState::Disabled => continue,
                DynamicTemperatureState::NotYetQueried { model } => model,
                DynamicTemperatureState::ValidAtLeastOnce { model, .. } => {
                    model
                }
            };

            // If there is a valid reading, update, otherwise leave in the
            // current state (either unqueried or the last valid value)
            if let Ok(r) = sensor_api.get_reading(di.sensor_id) {
                di.state = DynamicTemperatureState::ValidAtLeastOnce {
                    model,
                    reading: TimestampedTemperatureReading {
                        time_ms: r.timestamp,
                        value: Celsius(r.value),
                    },
                };
            }
        }
    }

    fn update_dynamic_input(
        &mut self,
        index: usize,
        model: ThermalProperties,
    ) -> Result<bool, ThermalError> {
        let Some(di) = self.dynamic_inputs.get_mut(index) else {
            return Err(ThermalError::InvalidIndex);
        };
        match di.state {
            DynamicTemperatureState::Disabled => {
                di.state = DynamicTemperatureState::NotYetQueried { model };
                Ok(true)
            }
            DynamicTemperatureState::NotYetQueried { .. }
            | DynamicTemperatureState::ValidAtLeastOnce { .. } => {
                // TODO: I think the old code just ignored this?
                Ok(false)
            }
        }
    }

    fn remove_dynamic_input(
        &mut self,
        index: usize,
    ) -> Result<SensorId, ThermalError> {
        let Some(di) = self.dynamic_inputs.get_mut(index) else {
            return Err(ThermalError::InvalidIndex);
        };
        // TODO: do we return an err if this was already removed? Check old code
        di.state = DynamicTemperatureState::Disabled;
        Ok(di.sensor_id)
    }

    fn all_inputs_queried(&self) -> bool {
        self.inputs.iter().all(InputChannel::has_been_queried)
            && self
                .dynamic_inputs
                .iter()
                .all(DynamicInputChannel::has_been_queried)
    }

    fn all_present_inputs_status(
        &self,
    ) -> impl Iterator<Item = InputStatus<'_>> {
        let inputs = self.inputs.iter().filter_map(|input| input.status());
        let dynamic_inputs =
            self.dynamic_inputs.iter().filter_map(|di| match &di.state {
                DynamicTemperatureState::Disabled => None,
                DynamicTemperatureState::NotYetQueried { .. } => None,
                DynamicTemperatureState::ValidAtLeastOnce {
                    model,
                    reading,
                } => Some(InputStatus {
                    sensor_id: di.sensor_id,
                    reading,
                    model,
                }),
            });

        inputs.chain(dynamic_inputs)
    }

    fn reset_all_values(&mut self) {
        let mode = self.power_mode();
        self.inputs.iter_mut().for_each(|i| i.reset_value(mode));
        self.dynamic_inputs
            .iter_mut()
            .for_each(|di| match di.state {
                DynamicTemperatureState::Disabled => {}
                DynamicTemperatureState::NotYetQueried { .. } => {}
                DynamicTemperatureState::ValidAtLeastOnce { model, .. } => {
                    di.state = DynamicTemperatureState::NotYetQueried { model };
                }
            });
    }

    fn set_all_watchdogs(
        &mut self,
        watchdog: drv_i2c_devices::max31790::I2cWatchdog,
    ) -> Result<(), ThermalError> {
        // Try setting both, NOT returning early if either failed
        let res_east = self
            .fctrl_east
            .try_initialize()
            .map_err(ThermalError::from)
            .and_then(|east| {
                east.set_watchdog(watchdog)
                    .map_err(|_| ThermalError::DeviceError)
            });
        let res_west = self
            .fctrl_west
            .try_initialize()
            .map_err(ThermalError::from)
            .and_then(|west| {
                west.set_watchdog(watchdog)
                    .map_err(|_| ThermalError::DeviceError)
            });

        match (res_east, res_west) {
            (Err(e), _) => Err(e),
            (_, Err(w)) => Err(w),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn set_all_fan_duty(
        &mut self,
        duty: userlib::units::PWMDuty,
    ) -> Result<(), ThermalError> {
        let mut any_err = false;
        let mut set_all = |fctrl: &mut Max31790, fans: &mut [Fan]| {
            for fan in fans.iter_mut() {
                let val = if !fan.is_present {
                    userlib::units::PWMDuty(0)
                } else {
                    duty
                };
                any_err |= fctrl.set_pwm(fan.bsp_data, val).is_err();
            }
        };

        // Load bearing assumption: the first 4 fans are the EAST fans, and the
        // last 4 fans are the WEST fans.
        let (east, west) = self.fans.split_at_mut(4);

        let mut init_err = false;
        if let Ok(fctrl) = self.fctrl_east.try_initialize() {
            set_all(fctrl, east);
        } else {
            init_err = true;
        }
        if let Ok(fctrl) = self.fctrl_west.try_initialize() {
            set_all(fctrl, west);
        } else {
            init_err = true;
        }

        if any_err | init_err {
            Err(ThermalError::DeviceError)
        } else {
            Ok(())
        }
    }
}

impl Bsp {
    pub fn new(i2c_task: TaskId) -> Self {
        // Handle for the sequencer task, which we check for power state and
        // fan presence
        let seq = Sequencer::from(SEQUENCER.get_task_id());

        let fctrl_east = Max31790State::new(&devices::max31790_east(i2c_task));
        let fctrl_west = Max31790State::new(&devices::max31790_west(i2c_task));

        static INPUTS_ONCE: static_cell::ClaimOnceCell<
            [InputChannel; NUM_TEMPERATURE_INPUTS],
        > = static_cell::ClaimOnceCell::new(INPUTS);

        static FANS_ONCE: static_cell::ClaimOnceCell<[Fan; NUM_FANS]> =
            static_cell::ClaimOnceCell::new(FANS);

        static DYN_INS_ONCE: static_cell::ClaimOnceCell<
            [DynamicInputChannel; NUM_DYNAMIC_TEMPERATURE_INPUTS],
        > = static_cell::ClaimOnceCell::new(DYNAMIC_INPUTS);

        Self {
            seq,
            fctrl_east,
            fctrl_west,

            // TODO: this is all made up, copied from tuned Gimlet values
            pid_config: PidConfig {
                zero: 35.0,
                gain_p: 1.75,
                gain_i: 0.0135,
                gain_d: 0.4,
                min_output: 0.0,
                max_output: 100.0,
            },

            inputs: INPUTS_ONCE.claim(),
            dynamic_inputs: DYN_INS_ONCE.claim(),

            // We monitor and log all of the air temperatures
            misc_sensors: &MISC_SENSORS,
            i2c_task,
            fans: FANS_ONCE.claim(),
        }
    }
}

//
// Guessing, big time
//
const TF2_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(60f32),
    critical_temperature: Celsius(70f32),
    power_down_temperature: Celsius(80f32),
    temperature_slew_deg_per_sec: 0.5,
};

// The VSC7448 has a maximum die temperature of 110°C, which is very
// hot.  Let's keep it a little cooler than that.
const VSC7448_THERMALS: ThermalProperties = ThermalProperties {
    target_temperature: Celsius(85f32),
    critical_temperature: Celsius(95f32),
    power_down_temperature: Celsius(105f32),
    temperature_slew_deg_per_sec: 0.5,
};

const INPUTS: [InputChannel; NUM_TEMPERATURE_INPUTS] = [
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Tmp451(Target::Remote),
            devices::tmp451_tf2,
            sensors::TMP451_TF2_TEMPERATURE_SENSOR,
        ),
        TF2_THERMALS,
        PowerBitmask::A0,
        ChannelType::MustBePresent,
    )),
    InputChannel::new(&InputChannelMetadata::new(
        TemperatureSensor::new(
            Device::Tmp451(Target::Remote),
            devices::tmp451_vsc7448,
            sensors::TMP451_VSC7448_TEMPERATURE_SENSOR,
        ),
        VSC7448_THERMALS,
        PowerBitmask::A0_OR_A2,
        ChannelType::MustBePresent,
    )),
];

const fn make_dynamic() -> [DynamicInputChannel; NUM_DYNAMIC_TEMPERATURE_INPUTS]
{
    const INIT: DynamicInputChannel =
        DynamicInputChannel::new(SensorId::new(0));
    let mut out = [INIT; NUM_DYNAMIC_TEMPERATURE_INPUTS];
    let mut idx = 0;
    while idx < NUM_DYNAMIC_TEMPERATURE_INPUTS {
        let sensor = drv_transceivers_api::TRANSCEIVER_TEMPERATURE_SENSORS[idx];
        out[idx] = DynamicInputChannel::new(sensor);
        idx += 1;
    }
    out
}
const DYNAMIC_INPUTS: [DynamicInputChannel; NUM_DYNAMIC_TEMPERATURE_INPUTS] =
    make_dynamic();

const MISC_SENSORS: [TemperatureSensor; NUM_TEMPERATURE_SENSORS] = [
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_northeast,
        sensors::TMP117_NORTHEAST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_nne,
        sensors::TMP117_NNE_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_nnw,
        sensors::TMP117_NNW_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_northwest,
        sensors::TMP117_NORTHWEST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_southeast,
        sensors::TMP117_SOUTHEAST_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_south,
        sensors::TMP117_SOUTH_TEMPERATURE_SENSOR,
    ),
    TemperatureSensor::new(
        Device::Tmp117,
        devices::tmp117_southwest,
        sensors::TMP117_SOUTHWEST_TEMPERATURE_SENSOR,
    ),
];

// Fan module 0/1 are on the east max31790; fan module 2/3 are on west
// max31790. Each fan module has two fans which are not mapped in a
// straightforward way. Additionally, our MAX31790 code has zero-indexed
// fan indices, but the part's datasheet and schematic symbol are
// one-indexed. Here is the mapping of the system level index to
// controller and fan index:
//
// System Index    Controller     Fan           MAX31790 Fan (Datasheet)
//     0            East           ESE           2 (3)
//     1            East           ENE           3 (4)
//     2            East           SE            0 (1)
//     3            East           NE            1 (2)
//     4            West           SW            2 (3)
//     5            West           NW            3 (4)
//     6            West           WSW           0 (1)
//     7            West           WNW           1 (2)
type Fan = crate::control::Fan<drv_i2c_devices::max31790::Fan>;
const FANS: [Fan; NUM_FANS] = [
    // EAST FANS
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[0],
        drv_i2c_devices::max31790::Fan::new_const(2),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[1],
        drv_i2c_devices::max31790::Fan::new_const(3),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[2],
        drv_i2c_devices::max31790::Fan::new_const(0),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[3],
        drv_i2c_devices::max31790::Fan::new_const(1),
    ),
    // WEST FANS
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[4],
        drv_i2c_devices::max31790::Fan::new_const(2),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[5],
        drv_i2c_devices::max31790::Fan::new_const(3),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[6],
        drv_i2c_devices::max31790::Fan::new_const(0),
    ),
    Fan::new(
        sensors::MAX31790_SPEED_SENSORS[7],
        drv_i2c_devices::max31790::Fan::new_const(1),
    ),
];
