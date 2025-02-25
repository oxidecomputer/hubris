// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the IMX3112 I2C mux

use crate::*;
use drv_i2c_api::{ResponseCode, Segment};

use bitfield::bitfield;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    DeviceTypeLo = 0x0,
    DeviceTypeHi = 0x1,
    DeviceRevision = 0x2,
    VendorIdLo = 0x3,
    VendorIdHi = 0x4,
    LocalInterfaceCfg = 0xe,
    PullupResistorConfig = 0xf,
    DeviceCfg = 0x12,
    ClearTempSensorAlarm = 0x13,
    ClearEccError = 0x14,
    TempSensorCfg = 0x1a,
    InterruptCfg = 0x1b,
    TempHiLimitCfgLo = 0x1c,
    TempHiLimitCfgHi = 0x1d,
    TempLoLimitCfgLo = 0x1e,
    TempLoLimitCfgHi = 0x1f,
    TempCritHiLimitCfgLo = 0x20,
    TempCritHiLimitCfgHi = 0x21,
    TempCritLoLimitCfgLo = 0x22,
    TempCritLoLimitCfgHi = 0x23,
    DeviceStatus = 0x30,
    CurrentTemperatureLo = 0x31,
    CurrentTemperatureHi = 0x32,
    TemperatureStatus = 0x33,
    ErrorStatus = 0x34,
    MuxConfig = 0x40,
    MuxSelect = 0x41,
}

bitfield! {
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct LocalInterfaceConfigRegister(u8);
    external_pullup, set_external_pullup: 5;
    ldo_voltage, set_ldo_voltage: 4, 3, 2;
}

bitfield! {
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct MuxSelectRegister(u8);
    channel1_enabled, set_channel1_enabled: 7;
    channel0_enabled, set_channel0_enabled: 6;
}

pub struct Imx3112;

fn write_reg_u8(
    mux: &I2cMux<'_>,
    controller: &I2cController<'_>,
    reg: Register,
    val: u8,
    ctrl: &I2cControl,
) -> Result<(), ResponseCode> {
    controller
        .write_read(
            mux.address,
            2,
            |pos| Some(if pos == 0 { reg as u8 } else { val }),
            ReadLength::Fixed(0),
            |_, _| Some(()),
            ctrl,
        )
        .map_err(|e| mux.error_code(e))
}

impl I2cMuxDriver for Imx3112 {
    fn configure(
        &self,
        mux: &I2cMux<'_>,
        controller: &I2cController<'_>,
        gpio: &sys_api::Sys,
        ctrl: &I2cControl,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        // Configure the mux to use external pull-ups
        mux.configure(gpio)?;

        let mut cfg = LocalInterfaceConfigRegister(0);
        cfg.set_external_pullup(true);
        write_reg_u8(
            mux,
            controller,
            Register::LocalInterfaceCfg,
            cfg.0,
            ctrl,
        )?;
        let mut reg = MuxSelectRegister(0);
        reg.set_channel0_enabled(true);
        write_reg_u8(mux, controller, Register::MuxConfig, 0, ctrl)?;
        write_reg_u8(mux, controller, Register::MuxSelect, reg.0, ctrl)?;
        write_reg_u8(mux, controller, Register::MuxConfig, reg.0, ctrl)?;

        Ok(())
    }

    fn enable_segment(
        &self,
        mux: &I2cMux<'_>,
        controller: &I2cController<'_>,
        segment: Option<Segment>,
        ctrl: &I2cControl,
    ) -> Result<(), ResponseCode> {
        let mut reg = MuxSelectRegister(0);
        match segment {
            Some(Segment::S1) => reg.set_channel0_enabled(true),
            Some(Segment::S2) => reg.set_channel1_enabled(true),
            None => (),
            _ => return Err(ResponseCode::SegmentNotFound),
        }
        // Enable only our desired output
        write_reg_u8(mux, controller, Register::MuxConfig, reg.0, ctrl)?;
        // Select our desired output
        write_reg_u8(mux, controller, Register::MuxSelect, reg.0, ctrl)?;
        Ok(())
    }

    fn reset(
        &self,
        mux: &I2cMux<'_>,
        gpio: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        mux.reset(gpio)
    }
}
