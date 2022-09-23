// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Power monitoring
//!
//! This is a primordial power monitoring task.
//!

#![no_std]
#![no_main]

use drv_i2c_devices::adm1272::*;
use drv_i2c_devices::bmr491::*;
use drv_i2c_devices::isl68224::*;
use drv_i2c_devices::raa229618::*;
use drv_i2c_devices::tps546b24a::*;
use task_sensor_api as sensor_api;
use userlib::units::*;
use userlib::*;

use drv_i2c_api::ResponseCode;
use drv_i2c_devices::{CurrentSensor, TempSensor, VoltageSensor};

use sensor_api::{NoData, SensorId};

#[derive(Copy, Clone, PartialEq)]
enum PowerState {
    A0,
    A2,
}

task_slot!(I2C, i2c_driver);
task_slot!(SENSOR, sensor);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

use i2c_config::sensors;

#[allow(dead_code, clippy::upper_case_acronyms)]
enum Device {
    IBC(Bmr491),
    Core(Raa229618),
    SerDes(Isl68224),
    Mem(Raa229618),
    MemVpp(Isl68224),
    Sys(Tps546B24A),
    HotSwap(Adm1272),
    Fan(Adm1272),
}

struct PowerController {
    state: PowerState,
    device: Device,
    voltage: SensorId,
    current: SensorId,
    temperature: Option<SensorId>,
}

fn read_temperature<E, T: TempSensor<E>>(
    device: &T,
) -> Result<Celsius, ResponseCode>
where
    ResponseCode: From<E>,
{
    match device.read_temperature() {
        Ok(reading) => Ok(reading),
        Err(err) => {
            let err: ResponseCode = err.into();
            Err(err)
        }
    }
}

fn read_current<E, T: CurrentSensor<E>>(
    device: &T,
) -> Result<Amperes, ResponseCode>
where
    ResponseCode: From<E>,
{
    match device.read_iout() {
        Ok(reading) => Ok(reading),
        Err(err) => {
            let err: ResponseCode = err.into();
            Err(err)
        }
    }
}

fn read_voltage<E, T: VoltageSensor<E>>(
    device: &T,
) -> Result<Volts, ResponseCode>
where
    ResponseCode: From<E>,
{
    match device.read_vout() {
        Ok(reading) => Ok(reading),
        Err(err) => {
            let err: ResponseCode = err.into();
            Err(err)
        }
    }
}

impl PowerController {
    fn read_temperature(&self) -> Result<Celsius, ResponseCode> {
        match &self.device {
            Device::IBC(dev) => read_temperature(dev),
            Device::Core(dev) | Device::Mem(dev) => read_temperature(dev),
            Device::MemVpp(_) => panic!(),
            Device::SerDes(dev) => read_temperature(dev),
            Device::Sys(dev) => read_temperature(dev),
            Device::HotSwap(dev) | Device::Fan(dev) => read_temperature(dev),
        }
    }

    fn read_iout(&self) -> Result<Amperes, ResponseCode> {
        match &self.device {
            Device::IBC(dev) => read_current(dev),
            Device::Core(dev) | Device::Mem(dev) => read_current(dev),
            Device::MemVpp(dev) => read_current(dev),
            Device::SerDes(dev) => read_current(dev),
            Device::Sys(dev) => read_current(dev),
            Device::HotSwap(dev) | Device::Fan(dev) => read_current(dev),
        }
    }

    fn read_vout(&self) -> Result<Volts, ResponseCode> {
        match &self.device {
            Device::IBC(dev) => read_voltage(dev),
            Device::Core(dev) | Device::Mem(dev) => read_voltage(dev),
            Device::MemVpp(dev) => read_voltage(dev),
            Device::SerDes(dev) => read_voltage(dev),
            Device::Sys(dev) => read_voltage(dev),
            Device::HotSwap(dev) | Device::Fan(dev) => read_voltage(dev),
        }
    }
}

macro_rules! rail_controller {
    ($task:expr, $which:ident, $dev:ident, $rail:ident, $state:ident) => {
        paste::paste! {
            PowerController {
                state: PowerState::$state,
                device: Device::$which({
                    let (device, rail) = i2c_config::pmbus::$rail($task);
                    [<$dev:camel>]::new(&device, rail)
                }),
                voltage: sensors::[<$dev:upper _ $rail:upper _VOLTAGE_SENSOR>],
                current: sensors::[<$dev:upper _ $rail:upper _CURRENT_SENSOR>],
                temperature: Some(
                    sensors::[<$dev:upper _ $rail:upper _TEMPERATURE_SENSOR>]
                ),
            }
        }
    };
}

#[allow(unused_macros)]
macro_rules! rail_controller_notemp {
    ($task:expr, $which:ident, $dev:ident, $rail:ident, $state:ident) => {
        paste::paste! {
            PowerController {
                state: PowerState::$state,
                device: Device::$which({
                    let (device, rail) = i2c_config::pmbus::$rail($task);
                    [<$dev:camel>]::new(&device, rail)
                }),
                voltage: sensors::[<$dev:upper _ $rail:upper _VOLTAGE_SENSOR>],
                current: sensors::[<$dev:upper _ $rail:upper _CURRENT_SENSOR>],
                temperature: None,
            }
        }
    };
}

#[allow(unused_macros)]
macro_rules! adm1272_controller {
    ($task:expr, $which:ident, $rail:ident, $state:ident, $rsense:expr) => {
        paste::paste! {
            PowerController {
                state: PowerState::$state,
                device: Device::$which({
                    let (device, _) = i2c_config::pmbus::$rail($task);
                    Adm1272::new(&device, $rsense)
                }),
                voltage: sensors::[<ADM1272_ $rail:upper _VOLTAGE_SENSOR>],
                current: sensors::[<ADM1272_ $rail:upper _CURRENT_SENSOR>],
                temperature: Some(
                    sensors::[<ADM1272_ $rail:upper _TEMPERATURE_SENSOR>]
                ),
            }
        }
    };
}

#[cfg(target_board = "gimlet-a")]
fn controllers() -> [PowerController; 13] {
    let task = I2C.get_task_id();

    [
        rail_controller!(task, IBC, bmr491, v12_sys_a2, A2),
        rail_controller!(task, Core, raa229618, vdd_vcore, A0),
        rail_controller!(task, Core, raa229618, vddcr_soc, A0),
        rail_controller!(task, Mem, raa229618, vdd_mem_abcd, A0),
        rail_controller!(task, Mem, raa229618, vdd_mem_efgh, A0),
        rail_controller_notemp!(task, MemVpp, isl68224, vpp_abcd, A0),
        rail_controller_notemp!(task, MemVpp, isl68224, vpp_efgh, A0),
        rail_controller_notemp!(task, MemVpp, isl68224, v3p3_sys, A0),
        rail_controller!(task, Sys, tps546B24A, v3p3_sp_a2, A2),
        rail_controller!(task, Sys, tps546B24A, v1p8_sp3, A0),
        rail_controller!(task, Sys, tps546B24A, v5_sys_a2, A2),
        adm1272_controller!(task, HotSwap, v54_hs_output, A2, Ohms(0.001)),
        adm1272_controller!(task, Fan, v54_fan, A2, Ohms(0.002)),
    ]
}

#[cfg(target_board = "gimlet-b")]
fn controllers() -> [PowerController; 15] {
    let task = I2C.get_task_id();

    [
        rail_controller!(task, IBC, bmr491, v12_sys_a2, A2),
        rail_controller!(task, Core, raa229618, vdd_vcore, A0),
        rail_controller!(task, Core, raa229618, vddcr_soc, A0),
        rail_controller!(task, Mem, raa229618, vdd_mem_abcd, A0),
        rail_controller!(task, Mem, raa229618, vdd_mem_efgh, A0),
        rail_controller_notemp!(task, MemVpp, isl68224, vpp_abcd, A0),
        rail_controller_notemp!(task, MemVpp, isl68224, vpp_efgh, A0),
        rail_controller_notemp!(task, MemVpp, isl68224, v1p8_sp3, A0),
        rail_controller!(task, Sys, tps546B24A, v3p3_sp_a2, A2),
        rail_controller!(task, Sys, tps546B24A, v3p3_sys_a0, A0),
        rail_controller!(task, Sys, tps546B24A, v5_sys_a2, A2),
        rail_controller!(task, Sys, tps546B24A, v1p8_sys_a2, A2),
        rail_controller!(task, Sys, tps546B24A, v0p96_nic_vdd_a0hp, A0),
        adm1272_controller!(task, HotSwap, v54_hs_output, A2, Ohms(0.001)),
        adm1272_controller!(task, Fan, v54_fan, A2, Ohms(0.002)),
    ]
}

#[cfg(feature = "gimlet")]
fn get_state() -> PowerState {
    task_slot!(SEQUENCER, gimlet_seq);

    use drv_gimlet_seq_api as seq_api;

    let sequencer = seq_api::Sequencer::from(SEQUENCER.get_task_id());

    //
    // We deliberately enumerate all power states to force the addition of
    // new ones to update this code.
    //
    match sequencer.get_state().unwrap() {
        seq_api::PowerState::A0
        | seq_api::PowerState::A0PlusHP
        | seq_api::PowerState::A0Thermtrip => PowerState::A0,
        seq_api::PowerState::A1
        | seq_api::PowerState::A2
        | seq_api::PowerState::A2PlusMono
        | seq_api::PowerState::A2PlusFans => PowerState::A2,
    }
}

#[cfg(target_board = "sidecar-a")]
fn controllers() -> [PowerController; 15] {
    let task = I2C.get_task_id();

    [
        rail_controller!(task, IBC, bmr491, v12p0_sys, A2),
        adm1272_controller!(task, Fan, v54_fan0, A2, Ohms(0.001)),
        adm1272_controller!(task, Fan, v54_fan1, A2, Ohms(0.001)),
        adm1272_controller!(task, Fan, v54_fan2, A2, Ohms(0.001)),
        adm1272_controller!(task, Fan, v54_fan3, A2, Ohms(0.001)),
        adm1272_controller!(task, Fan, v54_hsc, A2, Ohms(0.001)),
        rail_controller!(task, Core, raa229618, v0p8_tf2_vdd_core, A0),
        rail_controller!(task, Sys, tps546B24A, v3p3_sys, A2),
        rail_controller!(task, Sys, tps546B24A, v5p0_sys, A2),
        rail_controller!(task, Core, raa229618, v1p5_tf2_vdda, A0),
        rail_controller!(task, Core, raa229618, v0p9_tf2_vddt, A0),
        rail_controller!(task, SerDes, isl68224, v1p8_tf2_vdda, A0),
        rail_controller!(task, SerDes, isl68224, v1p8_tf2_vdd, A0),
        rail_controller!(task, Sys, tps546B24A, v1p0_mgmt, A2),
        rail_controller!(task, Sys, tps546B24A, v1p8_sys, A2),
    ]
}

#[cfg(target_board = "sidecar-a")]
fn get_state() -> PowerState {
    task_slot!(SEQUENCER, sequencer);

    use drv_sidecar_seq_api as seq_api;

    let sequencer = seq_api::Sequencer::from(SEQUENCER.get_task_id());

    match sequencer.tofino_seq_state() {
        Ok(seq_api::TofinoSeqState::A0) => PowerState::A0,
        Ok(seq_api::TofinoSeqState::A2) => PowerState::A2,
        _ => {
            panic!("bad state");
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let sensor = sensor_api::Sensor::from(SENSOR.get_task_id());

    let mut controllers = controllers();

    loop {
        hl::sleep_for(1000);

        let state = get_state();

        for c in &mut controllers {
            if c.state == PowerState::A0 && state != PowerState::A0 {
                sensor.nodata(c.voltage, NoData::DeviceOff).unwrap();
                sensor.nodata(c.current, NoData::DeviceOff).unwrap();

                if let Some(id) = c.temperature {
                    sensor.nodata(id, NoData::DeviceOff).unwrap();
                }

                continue;
            }

            if let Some(id) = c.temperature {
                match c.read_temperature() {
                    Ok(reading) => {
                        sensor.post(id, reading.0).unwrap();
                    }
                    Err(_) => {
                        sensor.nodata(id, NoData::DeviceError).unwrap();
                    }
                }
            }

            match c.read_iout() {
                Ok(reading) => {
                    sensor.post(c.current, reading.0).unwrap();
                }
                Err(_) => {
                    sensor.nodata(c.current, NoData::DeviceError).unwrap();
                }
            }

            match c.read_vout() {
                Ok(reading) => {
                    sensor.post(c.voltage, reading.0).unwrap();
                }
                Err(_) => {
                    sensor.nodata(c.voltage, NoData::DeviceError).unwrap();
                }
            }
        }
    }
}
