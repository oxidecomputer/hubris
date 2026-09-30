// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the AMD Versal SYSMON interface, as specified by AMD reference
//! document AM006.

use core::cell::Cell;

use crate::{
    BadValidation, TempSensor, Validate, VoltageSensor, pmbus_validate,
};
use drv_i2c_api::*;
use pmbus::{
    CommandCode,
    commands::{VOUT_MODE, am006},
};
use userlib::units::*;
use zerocopy::{Immutable, IntoBytes};

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, IntoBytes, Immutable)]
#[repr(u8)]
pub enum Register {
    // (0x00, "PAGE", WriteByte, ReadByte),
    Page = 0x00,
    // (0x03, "CLEAR_FAULTS", SendByte, Illegal),
    ClearFaults = 0x03,
    // (0x19, "CAPABILITY", Illegal, ReadByte),
    Capability = 0x19,
    // (0x20, "VOUT_MODE", WriteByte, ReadByte),
    VoutMode = 0x20,
    // (0x40, "VOUT_OV_FAULT_LIMIT", WriteWord, ReadWord),
    VoutOvFaultLimit = 0x40,
    // (0x44, "VOUT_UV_FAULT_LIMIT", WriteWord, ReadWord),
    VoutUvFaultLimit = 0x44,
    // (0x4f, "OT_FAULT_LIMIT", WriteWord, ReadWord),
    OtFaultLimit = 0x4f,
    // (0x51, "OT_WARN_LIMIT", WriteWord, ReadWord),
    OtWarnLimit = 0x51,
    // (0x52, "UT_WARN_LIMIT", WriteWord, ReadWord),
    UtWarnLimit = 0x52,
    // (0x53, "UT_FAULT_LIMIT", WriteWord, ReadWord),
    UtFaultLimit = 0x53,
    // (0x78, "STATUS_BYTE", WriteByte, ReadByte),
    StatusByte = 0x78,
    // (0x79, "STATUS_WORD", WriteWord, ReadWord),
    StatusWord = 0x79,
    // (0x7a, "STATUS_VOUT", WriteByte, ReadByte),
    StatusVout = 0x7a,
    // (0x7d, "STATUS_TEMPERATURE", WriteByte, ReadByte),
    StatusTemperature = 0x7d,
    // (0x7e, "STATUS_CML", WriteByte, ReadByte),
    StatusCml = 0x7e,
    // (0x8b, "READ_VOUT", Illegal, ReadWord),
    ReadVout = 0x8b,
    // (0x8d, "READ_TEMPERATURE_1", Illegal, ReadWord),
    ReadTemperature1 = 0x8d,
    // (0x98, "PMBUS_REVISION", Illegal, ReadByte),
    PmbusRevision = 0x98,
    // (0x99, "MFR_ID", WriteBlock, ReadBlock),
    MfrId = 0x99,
    // (0x9a, "MFR_MODEL", WriteBlock, ReadBlock),
    MfrModel = 0x9a,
    // (0x9b, "MFR_REVISION", WriteBlock, ReadBlock),
    MfrRevision = 0x9b,
    // (0xd0, "MFR_SPECIFIC_D0", MfrDefined, MfrDefined),
    MfrSpecificD0 = 0xd0,
    // (0xd1, "MFR_SPECIFIC_D1", MfrDefined, MfrDefined),
    MfrSpecificD1 = 0xd1,
    // (0xd2, "MFR_SPECIFIC_D2", WriteWord, ReadWord),
    MfrSpecificD2 = 0xd2,
    // (0xd3, "MFR_SPECIFIC_D3", WriteWord, ReadWord),
    MfrSpecificD3 = 0xd3,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// I2C error on PMBus read from device
    BadRead { cmd: u8, code: ResponseCode },

    /// I2C error on PMBus write to device
    BadWrite { cmd: u8, code: ResponseCode },

    /// Failed to parse PMBus data from device
    BadData { cmd: u8 },

    /// I2C error attempting to validate device
    BadValidation { cmd: u8, code: ResponseCode },

    /// PMBus data returned from device is invalid
    InvalidData { err: pmbus::Error },
}

impl From<BadValidation> for Error {
    fn from(value: BadValidation) -> Self {
        Self::BadValidation {
            cmd: value.cmd,
            code: value.code,
        }
    }
}

impl From<pmbus::Error> for Error {
    fn from(err: pmbus::Error) -> Self {
        Error::InvalidData { err }
    }
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRead { code, .. } => code,
            Error::BadWrite { code, .. } => code,
            Error::BadValidation { code, .. } => code,
            Error::BadData { .. } | Error::InvalidData { .. } => {
                ResponseCode::BadDeviceState
            }
        }
    }
}

pub struct Am006 {
    device: I2cDevice,
    mode: Cell<Option<pmbus::VOutModeCommandData>>,
}

impl core::fmt::Display for Am006 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "am006: {}", &self.device)
    }
}

impl Am006 {
    pub fn new(device: &I2cDevice) -> Self {
        // By default, the chip runs at 16 conversions per second, which is
        // plenty fast for our use case.
        Self {
            device: *device,
            mode: Cell::new(None),
        }
    }

    pub fn read_mode(&self) -> Result<pmbus::VOutModeCommandData, Error> {
        Ok(match self.mode.get() {
            None => {
                let mode = pmbus_read!(self.device, VOUT_MODE)?;
                self.mode.set(Some(mode));
                mode
            }
            Some(mode) => mode,
        })
    }
}

impl Validate<Error> for Am006 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = &[0x00, 0x00, 0x93];
        pmbus_validate(device, CommandCode::MFR_ID, expected)
            .map_err(Into::into)
    }
}

impl TempSensor<Error> for Am006 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let temp = pmbus_read!(self.device, am006::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl VoltageSensor<Error> for Am006 {
    fn read_vout(&self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, am006::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}
