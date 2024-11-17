// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! MWOCP68-3600 Murata power shelf

use crate::{
    pmbus_validate, BadValidation, CurrentSensor, InputCurrentSensor,
    InputVoltageSensor, Validate, VoltageSensor,
};
use core::cell::Cell;
use drv_i2c_api::{I2cDevice, ResponseCode};
use pmbus::commands::mwocp68::*;
use pmbus::commands::CommandCode;
use pmbus::units::{Celsius, Rpm};
use pmbus::*;
use task_power_api::PmbusValue;
use userlib::units::{Amperes, Volts};

pub struct Mwocp68 {
    device: I2cDevice,

    /// The index represents PMBus rail when reading voltage / current, and
    /// the sensor index when reading temperature (0-2) or fan speed (0-1).
    index: u8,

    mode: Cell<Option<pmbus::VOutModeCommandData>>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadData { cmd: u8 },
    BadValidation { cmd: u8, code: ResponseCode },
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

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRead { code, .. } => code,
            Error::BadWrite { code, .. } => code,
            Error::BadValidation { code, .. } => code,
            _ => ResponseCode::BadDeviceState,
        }
    }
}

impl From<pmbus::Error> for Error {
    fn from(err: pmbus::Error) -> Self {
        Error::InvalidData { err }
    }
}

impl Mwocp68 {
    pub fn new(device: &I2cDevice, index: u8) -> Self {
        Mwocp68 {
            device: *device,
            index,
            mode: Cell::new(None),
        }
    }

    fn set_rail(&self) -> Result<(), Error> {
        let page = PAGE::CommandData(self.index);
        pmbus_write!(self.device, PAGE, page)
    }

    pub fn read_mode(&self) -> Result<pmbus::VOutModeCommandData, Error> {
        Ok(match self.mode.get() {
            None => {
                let mode = pmbus_read!(self.device, commands::VOUT_MODE)?;
                self.mode.set(Some(mode));
                mode
            }
            Some(mode) => mode,
        })
    }

    pub fn read_temperature(&self) -> Result<Celsius, Error> {
        // Temperatures are accessible on all pages
        let r = match self.index {
            0 => pmbus_read!(self.device, READ_TEMPERATURE_1)?.get()?,
            1 => pmbus_read!(self.device, READ_TEMPERATURE_2)?.get()?,
            2 => pmbus_read!(self.device, READ_TEMPERATURE_3)?.get()?,
            _ => {
                return Err(Error::InvalidData {
                    err: pmbus::Error::InvalidCode,
                })
            }
        };
        Ok(r)
    }

    pub fn read_speed(&self) -> Result<Rpm, Error> {
        let r = match self.index {
            0 => pmbus_read!(self.device, READ_FAN_SPEED_1)?.get()?,
            1 => pmbus_read!(self.device, READ_FAN_SPEED_2)?.get()?,
            _ => {
                return Err(Error::InvalidData {
                    err: pmbus::Error::InvalidCode,
                })
            }
        };
        Ok(r)
    }

    #[inline(always)]
    fn read_block<const N: usize>(
        &self,
        cmd: CommandCode,
    ) -> Result<PmbusValue, Error> {
        // We can't use static_assertions with const generics (yet), so use a
        // regular assert and hope that the compiler removes it since both of
        // these are known constants.
        assert!(N <= task_power_api::MAX_BLOCK_LEN);

        // Pass through to the non-generic implementation.
        self.read_block_impl(cmd, N)
    }

    #[inline(never)]
    fn read_block_impl(
        &self,
        cmd: CommandCode,
        len: usize,
    ) -> Result<PmbusValue, Error> {
        let cmd = cmd as u8;
        let mut data = [0; task_power_api::MAX_BLOCK_LEN];
        let len = self
            .device
            .read_block(cmd, &mut data[..len])
            .map_err(|code| Error::BadRead { cmd, code })?;
        Ok(PmbusValue::Block {
            data,
            len: len as u8,
        })
    }

    pub fn pmbus_read(
        &self,
        op: task_power_api::Operation,
    ) -> Result<PmbusValue, Error> {
        use task_power_api::Operation;

        self.set_rail()?;

        let val = match op {
            Operation::FanConfig1_2 => {
                let (val, width) =
                    pmbus_read!(self.device, FAN_CONFIG_1_2)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::FanCommand1 => PmbusValue::from(
                pmbus_read!(self.device, FAN_COMMAND_1)?.get()?,
            ),
            Operation::FanCommand2 => PmbusValue::from(
                pmbus_read!(self.device, FAN_COMMAND_1)?.get()?,
            ),
            Operation::IoutOcFaultLimit => PmbusValue::from(
                pmbus_read!(self.device, IOUT_OC_FAULT_LIMIT)?.get()?,
            ),
            Operation::IoutOcWarnLimit => PmbusValue::from(
                pmbus_read!(self.device, IOUT_OC_WARN_LIMIT)?.get()?,
            ),
            Operation::OtWarnLimit => PmbusValue::from(
                pmbus_read!(self.device, OT_WARN_LIMIT)?.get()?,
            ),
            Operation::IinOcWarnLimit => PmbusValue::from(
                pmbus_read!(self.device, IIN_OC_WARN_LIMIT)?.get()?,
            ),
            Operation::PoutOpWarnLimit => PmbusValue::from(
                pmbus_read!(self.device, POUT_OP_WARN_LIMIT)?.get()?,
            ),
            Operation::PinOpWarnLimit => PmbusValue::from(
                pmbus_read!(self.device, PIN_OP_WARN_LIMIT)?.get()?,
            ),
            Operation::StatusByte => {
                let (val, width) = pmbus_read!(self.device, STATUS_BYTE)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::StatusWord => {
                let (val, width) = pmbus_read!(self.device, STATUS_WORD)?.raw();
                assert_eq!(width.0, 16);
                PmbusValue::Raw16(val as u16)
            }
            Operation::StatusVout => {
                let (val, width) = pmbus_read!(self.device, STATUS_VOUT)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::StatusIout => {
                let (val, width) = pmbus_read!(self.device, STATUS_IOUT)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::StatusInput => {
                let (val, width) =
                    pmbus_read!(self.device, STATUS_INPUT)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::StatusTemperature => {
                let (val, width) =
                    pmbus_read!(self.device, STATUS_TEMPERATURE)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::StatusCml => {
                let (val, width) = pmbus_read!(self.device, STATUS_CML)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::StatusMfrSpecific => {
                let (val, width) =
                    pmbus_read!(self.device, STATUS_MFR_SPECIFIC)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::StatusFans1_2 => {
                let (val, width) =
                    pmbus_read!(self.device, STATUS_FANS_1_2)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::ReadEin => {
                self.read_block::<6>(CommandCode::READ_EIN)?
            }
            Operation::ReadEout => {
                self.read_block::<6>(CommandCode::READ_EOUT)?
            }
            Operation::ReadVin => {
                PmbusValue::from(pmbus_read!(self.device, READ_VIN)?.get()?)
            }
            Operation::ReadIin => {
                PmbusValue::from(pmbus_read!(self.device, READ_IIN)?.get()?)
            }
            Operation::ReadVcap => {
                let vcap = pmbus_read!(self.device, READ_VCAP)?;
                PmbusValue::from(vcap.get(self.read_mode()?)?)
            }
            Operation::ReadVout => {
                let vout = pmbus_read!(self.device, READ_VOUT)?;
                PmbusValue::from(vout.get(self.read_mode()?)?)
            }
            Operation::ReadIout => {
                PmbusValue::from(pmbus_read!(self.device, READ_IOUT)?.get()?)
            }
            Operation::ReadTemperature1 => PmbusValue::from(
                pmbus_read!(self.device, READ_TEMPERATURE_1)?.get()?,
            ),
            Operation::ReadTemperature2 => PmbusValue::from(
                pmbus_read!(self.device, READ_TEMPERATURE_2)?.get()?,
            ),
            Operation::ReadTemperature3 => PmbusValue::from(
                pmbus_read!(self.device, READ_TEMPERATURE_3)?.get()?,
            ),
            Operation::ReadFanSpeed1 => PmbusValue::from(
                pmbus_read!(self.device, READ_FAN_SPEED_1)?.get()?,
            ),
            Operation::ReadFanSpeed2 => PmbusValue::from(
                pmbus_read!(self.device, READ_FAN_SPEED_2)?.get()?,
            ),
            Operation::ReadPout => {
                PmbusValue::from(pmbus_read!(self.device, READ_POUT)?.get()?)
            }
            Operation::ReadPin => {
                PmbusValue::from(pmbus_read!(self.device, READ_PIN)?.get()?)
            }
            Operation::PmbusRevision => {
                let (val, width) =
                    pmbus_read!(self.device, PMBUS_REVISION)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
            Operation::MfrId => self.read_block::<9>(CommandCode::MFR_ID)?,
            Operation::MfrModel => {
                self.read_block::<17>(CommandCode::MFR_MODEL)?
            }
            Operation::MfrRevision => {
                self.read_block::<14>(CommandCode::MFR_REVISION)?
            }
            Operation::MfrLocation => {
                self.read_block::<5>(CommandCode::MFR_LOCATION)?
            }
            Operation::MfrDate => {
                self.read_block::<4>(CommandCode::MFR_DATE)?
            }
            Operation::MfrSerial => {
                self.read_block::<12>(CommandCode::MFR_SERIAL)?
            }
            Operation::MfrVinMin => {
                PmbusValue::from(pmbus_read!(self.device, MFR_VIN_MIN)?.get()?)
            }
            Operation::MfrVinMax => {
                PmbusValue::from(pmbus_read!(self.device, MFR_VIN_MAX)?.get()?)
            }
            Operation::MfrIinMax => {
                PmbusValue::from(pmbus_read!(self.device, MFR_IIN_MAX)?.get()?)
            }
            Operation::MfrPinMax => {
                PmbusValue::from(pmbus_read!(self.device, MFR_PIN_MAX)?.get()?)
            }
            Operation::MfrVoutMin => {
                let vout = pmbus_read!(self.device, MFR_VOUT_MIN)?;
                PmbusValue::from(vout.get(self.read_mode()?)?)
            }
            Operation::MfrVoutMax => {
                let vout = pmbus_read!(self.device, MFR_VOUT_MAX)?;
                PmbusValue::from(vout.get(self.read_mode()?)?)
            }
            Operation::MfrIoutMax => {
                PmbusValue::from(pmbus_read!(self.device, MFR_IOUT_MAX)?.get()?)
            }
            Operation::MfrPoutMax => {
                PmbusValue::from(pmbus_read!(self.device, MFR_POUT_MAX)?.get()?)
            }
            Operation::MfrTambientMax => PmbusValue::from(
                pmbus_read!(self.device, MFR_TAMBIENT_MAX)?.get()?,
            ),
            Operation::MfrTambientMin => PmbusValue::from(
                pmbus_read!(self.device, MFR_TAMBIENT_MIN)?.get()?,
            ),
            Operation::MfrEfficiencyHl => {
                self.read_block::<14>(CommandCode::MFR_EFFICIENCY_HL)?
            }
            Operation::MfrMaxTemp1 => PmbusValue::from(
                pmbus_read!(self.device, MFR_MAX_TEMP_1)?.get()?,
            ),
            Operation::MfrMaxTemp2 => PmbusValue::from(
                pmbus_read!(self.device, MFR_MAX_TEMP_2)?.get()?,
            ),
            Operation::MfrMaxTemp3 => PmbusValue::from(
                pmbus_read!(self.device, MFR_MAX_TEMP_3)?.get()?,
            ),
        };

        Ok(val)
    }

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
    }
}

impl Validate<Error> for Mwocp68 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = b"MWOCP68-3600-D-RM";
        pmbus_validate(device, CommandCode::MFR_MODEL, expected)
            .map_err(Into::into)
    }
}

impl VoltageSensor<Error> for Mwocp68 {
    fn read_vout(&self) -> Result<Volts, Error> {
        self.set_rail()?;
        let vout = pmbus_read!(self.device, READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}

impl CurrentSensor<Error> for Mwocp68 {
    fn read_iout(&self) -> Result<Amperes, Error> {
        self.set_rail()?;
        let iout = pmbus_read!(self.device, READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}

impl InputVoltageSensor<Error> for Mwocp68 {
    fn read_vin(&self) -> Result<Volts, Error> {
        self.set_rail()?;
        let vin = pmbus_read!(self.device, READ_VIN)?;
        Ok(Volts(vin.get()?.0))
    }
}

impl InputCurrentSensor<Error> for Mwocp68 {
    fn read_iin(&self) -> Result<Amperes, Error> {
        self.set_rail()?;
        let iin = pmbus_read!(self.device, READ_IIN)?;
        Ok(Amperes(iin.get()?.0))
    }
}
