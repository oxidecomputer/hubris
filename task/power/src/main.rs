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
use drv_i2c_devices::ltc4282::*;
use drv_i2c_devices::max5970::*;
use drv_i2c_devices::mwocp68::*;
use drv_i2c_devices::raa229618::*;
use drv_i2c_devices::tps546b24a::*;
use ringbuf::*;
use task_power_api::{Bmr491Event, PmbusValue, RenesasBlackbox};
use task_sensor_api as sensor_api;
use userlib::units::*;
use userlib::*;
use zerocopy::AsBytes;

use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_i2c_devices::{
    CurrentSensor, InputCurrentSensor, InputVoltageSensor, TempSensor,
    VoltageSensor,
};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    GotVersion(u32),
    GotAddr(u32),
    None,
}

ringbuf!(Trace, 16, Trace::None);

use sensor_api::{NoData, SensorId};

#[derive(Copy, Clone, PartialEq)]
enum PowerState {
    A0,
    A2,
}

const TIMER_INTERVAL: u64 = 1000;

task_slot!(I2C, i2c_driver);
task_slot!(SENSOR, sensor);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[allow(dead_code, clippy::upper_case_acronyms)]
#[derive(Clone, Copy)]
enum DeviceType {
    IBC,
    Core,
    SerDes,
    Mem,
    MemVpp,
    Sys,
    HotSwap(Ohms),
    Fan(Ohms),
    HotSwapIO(Ohms),
    HotSwapQSFP(Ohms),
    PowerShelf,
}

struct PowerControllerConfig {
    state: PowerState,
    device: DeviceType,
    builder: fn(TaskId) -> (drv_i2c_api::I2cDevice, u8), // device, rail
    voltage: SensorId,
    input_voltage: Option<SensorId>,
    current: SensorId,
    input_current: Option<SensorId>,
    temperature: Option<SensorId>,
}

enum Device {
    Bmr491(Bmr491),
    Raa229618(Raa229618),
    Isl68224(Isl68224),
    Tps546B24A(Tps546B24A),
    Adm1272(Adm1272),
    Max5970(Max5970),
    Mwocp68(Mwocp68),
    Ltc4282(Ltc4282),
}

impl Device {
    fn read_temperature(&self) -> Result<Celsius, ResponseCode> {
        let r = match &self {
            Device::Bmr491(dev) => dev.read_temperature()?,
            Device::Raa229618(dev) => dev.read_temperature()?,
            Device::Isl68224(dev) => dev.read_temperature()?,
            Device::Tps546B24A(dev) => dev.read_temperature()?,
            Device::Adm1272(dev) => dev.read_temperature()?,
            Device::Mwocp68(..) => {
                // The MWOCP68 actually has three temperature sensors, but they
                // aren't associated with power rails, so we don't read them
                // here.
                return Err(ResponseCode::NoDevice);
            }
            Device::Max5970(..) | Device::Ltc4282(..) => {
                return Err(ResponseCode::NoDevice);
            }
        };
        Ok(r)
    }

    fn read_iout(&self) -> Result<Amperes, ResponseCode> {
        let r = match &self {
            Device::Bmr491(dev) => dev.read_iout()?,
            Device::Raa229618(dev) => dev.read_iout()?,
            Device::Isl68224(dev) => dev.read_iout()?,
            Device::Tps546B24A(dev) => dev.read_iout()?,
            Device::Adm1272(dev) => dev.read_iout()?,
            Device::Max5970(dev) => dev.read_iout()?,
            Device::Mwocp68(dev) => dev.read_iout()?,
            Device::Ltc4282(dev) => dev.read_iout()?,
        };
        Ok(r)
    }

    fn read_vout(&self) -> Result<Volts, ResponseCode> {
        let r = match &self {
            Device::Bmr491(dev) => dev.read_vout()?,
            Device::Raa229618(dev) => dev.read_vout()?,
            Device::Isl68224(dev) => dev.read_vout()?,
            Device::Tps546B24A(dev) => dev.read_vout()?,
            Device::Adm1272(dev) => dev.read_vout()?,
            Device::Max5970(dev) => dev.read_vout()?,
            Device::Mwocp68(dev) => dev.read_vout()?,
            Device::Ltc4282(dev) => dev.read_vout()?,
        };
        Ok(r)
    }

    fn read_vin(&self) -> Result<Volts, ResponseCode> {
        let r = match &self {
            Device::Mwocp68(dev) => dev.read_vin()?,
            // Do any other devices have VIN? For now we only added support to
            // MWOCP68
            _ => return Err(ResponseCode::NoDevice),
        };
        Ok(r)
    }

    fn read_iin(&self) -> Result<Amperes, ResponseCode> {
        let r = match &self {
            Device::Mwocp68(dev) => dev.read_iin()?,
            // Do any other devices have IIN? For now we only added support to
            // MWOCP68
            _ => return Err(ResponseCode::NoDevice),
        };
        Ok(r)
    }

    fn pmbus_read(
        &self,
        op: task_power_api::Operation,
    ) -> Result<PmbusValue, ResponseCode> {
        let v = match &self {
            Device::Mwocp68(dev) => dev.pmbus_read(op)?,
            Device::Bmr491(_)
            | Device::Raa229618(_)
            | Device::Isl68224(_)
            | Device::Tps546B24A(_)
            | Device::Adm1272(_)
            | Device::Ltc4282(_)
            | Device::Max5970(_) => {
                return Err(ResponseCode::OperationNotSupported)
            }
        };
        Ok(v)
    }

    fn read_mode(&self) -> Result<pmbus::VOutModeCommandData, ResponseCode> {
        let v = match &self {
            Device::Mwocp68(dev) => dev.read_mode()?,
            Device::Bmr491(dev) => dev.read_mode()?,
            Device::Raa229618(dev) => dev.read_mode()?,
            Device::Isl68224(dev) => dev.read_mode()?,
            Device::Tps546B24A(dev) => dev.read_mode()?,
            Device::Adm1272(..) | Device::Ltc4282(..) | Device::Max5970(..) => {
                return Err(ResponseCode::OperationNotSupported)
            }
        };
        Ok(v)
    }

    fn i2c_device(&self) -> &I2cDevice {
        match &self {
            Device::Mwocp68(dev) => dev.i2c_device(),
            Device::Bmr491(dev) => dev.i2c_device(),
            Device::Raa229618(dev) => dev.i2c_device(),
            Device::Isl68224(dev) => dev.i2c_device(),
            Device::Tps546B24A(dev) => dev.i2c_device(),
            Device::Adm1272(dev) => dev.i2c_device(),
            Device::Ltc4282(dev) => dev.i2c_device(),
            Device::Max5970(dev) => dev.i2c_device(),
        }
    }
}

impl PowerControllerConfig {
    fn get_device(&self, task: TaskId) -> Device {
        let (dev, rail) = (self.builder)(task);
        match &self.device {
            DeviceType::IBC => Device::Bmr491(Bmr491::new(&dev, rail)),
            DeviceType::Core | DeviceType::Mem => {
                Device::Raa229618(Raa229618::new(&dev, rail))
            }
            DeviceType::MemVpp | DeviceType::SerDes => {
                Device::Isl68224(Isl68224::new(&dev, rail))
            }
            DeviceType::Sys => Device::Tps546B24A(Tps546B24A::new(&dev, rail)),
            DeviceType::HotSwap(sense) | DeviceType::Fan(sense) => {
                Device::Adm1272(Adm1272::new(&dev, *sense))
            }
            DeviceType::HotSwapIO(sense) => {
                Device::Max5970(Max5970::new(&dev, rail, *sense))
            }
            DeviceType::PowerShelf => Device::Mwocp68(Mwocp68::new(&dev, rail)),
            DeviceType::HotSwapQSFP(sense) => {
                Device::Ltc4282(Ltc4282::new(&dev, *sense))
            }
        }
    }
}

#[allow(unused_macros)]
macro_rules! rail_controller {
    ($which:ident, $dev:ident, $rail:ident, $state:ident) => {
        paste::paste! {
            PowerControllerConfig {
                state: PowerState::$state,
                device: DeviceType::$which,
                builder: i2c_config::pmbus::$rail,
                voltage: sensors::[<$dev:upper _ $rail:upper _VOLTAGE_SENSOR>],
                input_voltage: None,
                current: sensors::[<$dev:upper _ $rail:upper _CURRENT_SENSOR>],
                input_current: None,
                temperature: Some(
                    sensors::[<$dev:upper _ $rail:upper _TEMPERATURE_SENSOR>]
                ),
            }
        }
    };
}

#[allow(unused_macros)]
macro_rules! rail_controller_notemp {
    ($which:ident, $dev:ident, $rail:ident, $state:ident) => {
        paste::paste! {
            PowerControllerConfig {
                state: PowerState::$state,
                device: DeviceType::$which,
                builder:i2c_config::pmbus::$rail,
                voltage: sensors::[<$dev:upper _ $rail:upper _VOLTAGE_SENSOR>],
                input_voltage: None,
                current: sensors::[<$dev:upper _ $rail:upper _CURRENT_SENSOR>],
                input_current: None,
                temperature: None,
            }
        }
    };
}

#[allow(unused_macros)]
macro_rules! adm1272_controller {
    ($which:ident, $rail:ident, $state:ident, $rsense:expr) => {
        paste::paste! {
            PowerControllerConfig {
                state: PowerState::$state,
                device: DeviceType::$which($rsense),
                builder: i2c_config::pmbus::$rail,
                voltage: sensors::[<ADM1272_ $rail:upper _VOLTAGE_SENSOR>],
                input_voltage: None,
                current: sensors::[<ADM1272_ $rail:upper _CURRENT_SENSOR>],
                input_current: None,
                temperature: Some(
                    sensors::[<ADM1272_ $rail:upper _TEMPERATURE_SENSOR>]
                ),
            }
        }
    };
}

#[allow(unused_macros)]
macro_rules! ltc4282_controller {
    ($which:ident, $rail:ident, $state:ident, $rsense:expr) => {
        paste::paste! {
            PowerControllerConfig {
                state: PowerState::$state,
                device: DeviceType::$which($rsense),
                builder: i2c_config::power::$rail,
                voltage: sensors::[<LTC4282_ $rail:upper _VOLTAGE_SENSOR>],
                input_voltage: None,
                current: sensors::[<LTC4282_ $rail:upper _CURRENT_SENSOR>],
                input_current: None,
                temperature: None,
            }
        }
    };
}

#[allow(unused_macros)]
macro_rules! max5970_controller {
    ($which:ident, $rail:ident, $state:ident, $rsense:expr) => {
        paste::paste! {
            PowerControllerConfig {
                state: PowerState::$state,
                device: DeviceType::$which($rsense),
                builder: i2c_config::power::$rail,
                voltage: sensors::[<MAX5970_ $rail:upper _VOLTAGE_SENSOR>],
                input_voltage: None,
                current: sensors::[<MAX5970_ $rail:upper _CURRENT_SENSOR>],
                input_current: None,
                temperature: None,
            }
        }
    };
}

#[allow(unused_macros)]
macro_rules! mwocp68_controller {
    ($which:ident, $rail:ident, $state:ident) => {
        paste::paste! {
            PowerControllerConfig {
                state: PowerState::$state,
                device: DeviceType::$which,
                builder: i2c_config::pmbus::$rail,
                voltage: sensors::[<MWOCP68_ $rail:upper _VOLTAGE_SENSOR>],
                input_voltage: Some(
                    sensors::[<MWOCP68_ $rail:upper _INPUT_VOLTAGE_SENSOR>]
                ),
                current: sensors::[<MWOCP68_ $rail:upper _CURRENT_SENSOR>],
                input_current: Some(
                    sensors::[<MWOCP68_ $rail:upper _INPUT_CURRENT_SENSOR>]
                ),
                temperature: None, // Temperature sensors are independent of
                                   // power rails and measured separately
            }
        }
    };
}

////////////////////////////////////////////////////////////////////////////////
// Board-specific behavior is isolated into a `bsp` module, which is picked
// based on the target_board name.
#[cfg_attr(
    any(
        target_board = "gimlet-b",
        target_board = "gimlet-c",
        target_board = "gimlet-d"
    ),
    path = "bsp/gimlet_bcd.rs"
)]
#[cfg_attr(
    any(
        target_board = "psc-a",
        target_board = "psc-b",
        target_board = "psc-c"
    ),
    path = "bsp/psc_abc.rs"
)]
#[cfg_attr(
    any(target_board = "sidecar-b", target_board = "sidecar-c",),
    path = "bsp/sidecar_bc.rs"
)]
#[cfg_attr(target_board = "gimletlet-2", path = "bsp/gimletlet_2.rs")]
mod bsp;

////////////////////////////////////////////////////////////////////////////////

#[export_name = "main"]
fn main() -> ! {
    bsp::preinit();

    let i2c_task = I2C.get_task_id();

    let mut server = ServerImpl {
        i2c_task,
        sensor: sensor_api::Sensor::from(SENSOR.get_task_id()),
        devices: claim_devices(i2c_task),
    };
    let mut buffer = [0; idl::INCOMING_SIZE];

    sys_set_timer(
        Some(sys_get_timer().now + TIMER_INTERVAL),
        notifications::TIMER_MASK,
    );
    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    i2c_task: TaskId,
    sensor: sensor_api::Sensor,
    devices: &'static mut [Device; bsp::CONTROLLER_CONFIG.len()],
}

impl ServerImpl {
    fn handle_timer_fired(&mut self) {
        let state = bsp::get_state();
        let sensor = &self.sensor;

        for (c, dev) in
            bsp::CONTROLLER_CONFIG.iter().zip(self.devices.iter_mut())
        {
            if c.state == PowerState::A0 && state != PowerState::A0 {
                let now = sys_get_timer().now;
                sensor.nodata(c.voltage, NoData::DeviceOff, now).unwrap();
                sensor.nodata(c.current, NoData::DeviceOff, now).unwrap();

                if let Some(id) = c.temperature {
                    sensor.nodata(id, NoData::DeviceOff, now).unwrap();
                }

                continue;
            }

            if let Some(id) = c.temperature {
                match dev.read_temperature() {
                    Ok(reading) => {
                        sensor.post_now(id, reading.0).unwrap();
                    }
                    Err(_) => {
                        sensor.nodata_now(id, NoData::DeviceError).unwrap();
                    }
                }
            }

            match dev.read_iout() {
                Ok(reading) => {
                    sensor.post_now(c.current, reading.0).unwrap();
                }
                Err(_) => {
                    sensor.nodata_now(c.current, NoData::DeviceError).unwrap();
                }
            }

            match dev.read_vout() {
                Ok(reading) => {
                    sensor.post_now(c.voltage, reading.0).unwrap();
                }
                Err(_) => {
                    sensor.nodata_now(c.voltage, NoData::DeviceError).unwrap();
                }
            }

            if let Some(id) = c.input_voltage {
                match dev.read_vin() {
                    Ok(reading) => {
                        sensor.post_now(id, reading.0).unwrap();
                    }
                    Err(_) => {
                        sensor.nodata_now(id, NoData::DeviceError).unwrap();
                    }
                }
            }

            if let Some(id) = c.input_current {
                match dev.read_iin() {
                    Ok(reading) => {
                        sensor.post_now(id, reading.0).unwrap();
                    }
                    Err(_) => {
                        sensor.nodata_now(id, NoData::DeviceError).unwrap();
                    }
                }
            }
        }
    }

    /// Find the BMR491 and return an `I2cDevice` handle
    ///
    /// This could be a _little_ inefficient, but means that the code doesn't
    /// need to be special-cased for SPs without a BMR491 (and it's the first
    /// item in the list anyways).
    fn bmr491(&self) -> Result<I2cDevice, ResponseCode> {
        let device = bsp::CONTROLLER_CONFIG
            .iter()
            .find(|dev| matches!(dev.device, DeviceType::IBC))
            .ok_or(ResponseCode::NoDevice)?;
        let (dev, _rail) = (device.builder)(self.i2c_task);
        Ok(dev)
    }

    fn get_device(
        &self,
        req_dev: task_power_api::Device,
        req_rail: u8,
        req_index: u32,
    ) -> Result<Device, ResponseCode> {
        use task_power_api::Device;

        // Skim through `CONTROLLER_CONFIG` looking for the requested device.
        bsp::CONTROLLER_CONFIG
            .iter()
            .filter_map(|dev| {
                match (dev.device, req_dev) {
                    // Filter down to only devices that match types...
                    (DeviceType::PowerShelf, Device::PowerShelf) => {
                        let (_device, rail) = (dev.builder)(self.i2c_task);
                        // ... and rails
                        (rail == req_rail)
                            .then_some(dev.get_device(self.i2c_task))
                    }
                    (DeviceType::IBC, Device::Bmr491) => {
                        let (_device, rail) = (dev.builder)(self.i2c_task);
                        // ... and rails
                        (rail == req_rail)
                            .then_some(dev.get_device(self.i2c_task))
                    }
                    _ => None,
                }
            })
            .nth(req_index as usize)
            .ok_or(ResponseCode::NoDevice)
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        self.handle_timer_fired();
        sys_set_timer(
            Some(sys_get_timer().now + TIMER_INTERVAL),
            notifications::TIMER_MASK,
        );
    }
}

impl idl::InOrderPowerImpl for ServerImpl {
    fn pmbus_read(
        &mut self,
        _msg: &userlib::RecvMessage,
        req_dev: task_power_api::Device,
        req_rail: u8,
        req_index: u32,
        op: task_power_api::Operation,
    ) -> Result<PmbusValue, idol_runtime::RequestError<ResponseCode>> {
        let device = self.get_device(req_dev, req_rail, req_index)?;
        Ok(device.pmbus_read(op)?)
    }

    fn read_mode(
        &mut self,
        _msg: &userlib::RecvMessage,
        req_dev: task_power_api::Device,
        req_rail: u8,
        req_index: u32,
    ) -> Result<u8, idol_runtime::RequestError<ResponseCode>> {
        let device = self.get_device(req_dev, req_rail, req_index)?;
        let out = device.read_mode()?;
        Ok(out.0)
    }

    fn bmr491_event_log_read(
        &mut self,
        _msg: &userlib::RecvMessage,
        index: u8,
    ) -> Result<Bmr491Event, idol_runtime::RequestError<ResponseCode>> {
        // The BMR491 has 48 event log slots:
        // - 0-23 are reserved for faults
        // - 24-47 are reserved for lifecycle events
        if index >= 48 {
            return Err(ResponseCode::BadArg.into());
        }

        let dev = self.bmr491()?;
        dev.write(&[
            pmbus::commands::bmr491::CommandCode::MFR_EVENT_INDEX as u8,
            index,
        ])?;

        let out = dev.read_reg(
            pmbus::commands::bmr491::CommandCode::MFR_READ_EVENT as u8,
        )?;

        Ok(out)
    }

    fn bmr491_max_fault_event_index(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<u8, idol_runtime::RequestError<ResponseCode>> {
        let dev = self.bmr491()?;

        // 255 is a special value, setting MFR_EVENT_INDEX to the index of the
        // newest record in the fault section of the event recorder.
        dev.write(&[
            pmbus::commands::bmr491::CommandCode::MFR_EVENT_INDEX as u8,
            255,
        ])?;

        let out = dev.read_reg(
            pmbus::commands::bmr491::CommandCode::MFR_EVENT_INDEX as u8,
        )?;
        Ok(out)
    }

    fn bmr491_max_lifecycle_event_index(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<u8, idol_runtime::RequestError<ResponseCode>> {
        let dev = self.bmr491()?;

        // 254 is *also* a special value, setting MFR_EVENT_INDEX to the index
        // of the newest record in the lifecycle event section of the event
        // recorder.
        dev.write(&[
            pmbus::commands::bmr491::CommandCode::MFR_EVENT_INDEX as u8,
            254,
        ])?;

        let out = dev.read_reg(
            pmbus::commands::bmr491::CommandCode::MFR_EVENT_INDEX as u8,
        )?;
        Ok(out)
    }

    fn rendmp_blackbox_dump(
        &mut self,
        _msg: &userlib::RecvMessage,
        addr: u8,
    ) -> Result<RenesasBlackbox, idol_runtime::RequestError<ResponseCode>> {
        if !bsp::HAS_RENDMP_BLACKBOX {
            return Err(ResponseCode::OperationNotSupported.into());
        }

        let dev = self
            .devices
            .iter()
            .find(|d| d.i2c_device().address == addr)
            .ok_or(ResponseCode::NoDevice)?;

        // The isl68224 and raa229618 have identical DMAADDR / DMAFIX / DMASEQ
        // command codes, which we'll check with a static assertion here.
        static_assertions::const_assert_eq!(
            pmbus::commands::isl68224::CommandCode::DMAADDR as u8,
            pmbus::commands::raa229618::CommandCode::DMAADDR as u8
        );
        static_assertions::const_assert_eq!(
            pmbus::commands::isl68224::CommandCode::DMAFIX as u8,
            pmbus::commands::raa229618::CommandCode::DMAFIX as u8
        );
        static_assertions::const_assert_eq!(
            pmbus::commands::isl68224::CommandCode::DMASEQ as u8,
            pmbus::commands::raa229618::CommandCode::DMASEQ as u8
        );

        // Now that we've proven equivalence, let's import this namespace
        use pmbus::commands::isl68224::CommandCode;

        // Our two chips are from two different generations:
        // - The RAA229618 uses the "Gen 2.5" guide to get the blackbox; see
        //   Renesas_DMP_Gen2p5_BlackBox_RAM.pdf for details
        // - The ISL68224 uses the "Gen 2" guide; see
        //   Renesas_DMP_Gen2_BlackBox_RAM.pdf
        //
        // However, it turns out that the procedures are identical except for
        // three things:
        // - The address register checked in Step 2
        // - The expected ID of the part
        // - The size of the blackbox
        //
        // We extract those differences here, then use a single codepath for
        // both generations of parts.
        let (mut out, expected_id) = match dev {
            Device::Raa229618(..) => {
                (RenesasBlackbox::Gen2p5([0u32; 44]), 0x2000001)
            }
            Device::Isl68224(..) => {
                (RenesasBlackbox::Gen2([0u32; 38]), 0x2000004)
            }
            // No one else has a blackbox
            _ => return Err(ResponseCode::NoDevice.into()),
        };
        let addr_reg = out.addr_reg();
        let dev = dev.i2c_device();

        // Step 1 - Verify Part Revision
        let mut id = 0u32;
        dev.read_block(CommandCode::IC_DEVICE_REV as u8, id.as_bytes_mut())?;
        ringbuf_entry!(Trace::GotVersion(id));

        // Experimentally determined ID
        if id != expected_id {
            return Err(ResponseCode::OperationNotSupported.into());
        }

        // Step 2a - Write to DMA Address Register
        dev.write(&[CommandCode::DMAADDR as u8, addr_reg, 0x00])?;

        // Step 2b - Read DMA Data Register
        let mut r: u32 = dev.read_reg(CommandCode::DMAFIX as u8)?;
        ringbuf_entry!(Trace::GotAddr(r));

        // "Divide this value by 4 to determine the starting address of the
        //  Black Box data."
        r /= 4;

        // This address is written as a 2-byte value, so I'm assuming
        // something has gone wrong if our result doesn't fit.
        if r > u16::MAX as u32 {
            return Err(ResponseCode::BadResponse.into());
        }

        // Step 3a - Write to DMA Address Register
        dev.write(&[CommandCode::DMAADDR as u8, r as u8, (r >> 8) as u8])?;

        // Step 3b - Read Black Box Data
        let buf = match &mut out {
            RenesasBlackbox::Gen2(buf) => buf.as_mut_slice(),
            RenesasBlackbox::Gen2p5(buf) => buf.as_mut_slice(),
        };
        for b in buf {
            let r: u32 = dev.read_reg(CommandCode::DMASEQ as u8)?;
            *b = r.swap_bytes();
        }

        Ok(out)
    }

    fn rendmp_dma_read(
        &mut self,
        _msg: &userlib::RecvMessage,
        addr: u8,
        reg: u16,
    ) -> Result<u32, idol_runtime::RequestError<ResponseCode>> {
        let dev = self
            .devices
            .iter()
            .find(|d| {
                d.i2c_device().address == addr
                    && matches!(d, Device::Raa229618(..) | Device::Isl68224(..))
            })
            .ok_or(ResponseCode::NoDevice)?
            .i2c_device();

        use pmbus::commands::isl68224::CommandCode;
        dev.write(&[CommandCode::DMAADDR as u8, reg as u8, (reg >> 8) as u8])?;
        let out: u32 = dev.read_reg(CommandCode::DMAFIX as u8)?;
        Ok(out)
    }
}

/// Claims a mutable buffer of Devices, built from CONTROLLER_CONFIG.
///
/// This function can only be called once, and will panic otherwise!
fn claim_devices(
    i2c_task: TaskId,
) -> &'static mut [Device; bsp::CONTROLLER_CONFIG.len()] {
    let mut iter = bsp::CONTROLLER_CONFIG.iter();
    let dev = mutable_statics::mutable_statics!(
        static mut DEVICES: [Device; bsp::CONTROLLER_CONFIG.len()] =
            [|| iter.next().unwrap().get_device(i2c_task); _];
    );
    dev
}

mod idl {
    use task_power_api::*;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
