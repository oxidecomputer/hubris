// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! MWOCP67-5500 Murata power shelf

use crate::{
    BadValidation, CurrentSensor, InputCurrentSensor, InputVoltageSensor,
    Validate, VoltageSensor, pmbus_validate,
};
use core::cell::Cell;
use drv_i2c_api::*;
use fixedstr::FixedString;
use pmbus::commands::CommandCode;
use pmbus::commands::mwocp67::*;
use pmbus::units::{Celsius, Rpm};
use pmbus::*;
use ringbuf::*;
use task_power_api::PmbusValue;
use userlib::UnwrapLite;
use userlib::units::{Amperes, Volts};

pub struct Mwocp67 {
    device: I2cDevice,

    /// The index represents PMBus rail when reading voltage / current,
    /// the sensor index when reading temperature (0-4), and is ignored when
    /// reading the speed of the single fan.
    index: u8,

    mode: Cell<Option<pmbus::VOutModeCommandData>>,
}

#[derive(Copy, Clone, PartialEq)]
pub struct FirmwareRev(pub [u8; 4]);

#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub struct SerialNumber(pub [u8; 12]);

/// Manufacturer model number.
///
/// Per Murata Application Note ACAN-157 "PMBus Communication Protocol",
/// this is always a 17-byte ASCII string. It should be "MWOCP67-5500-B-RM".
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub struct ModelNumber(pub [u8; 17]);

/// Manufacturer ID.
///
/// Per Murata Application Note ACAN-157 "PMBus Communication Protocol",
/// this is always a 9-byte ASCII string. It should be "Murata-PS".
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub struct MfrId(pub [u8; 9]);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    BadRead { cmd: u8, code: ResponseCode },
    BadWrite { cmd: u8, code: ResponseCode },
    BadData { cmd: u8 },
    BadValidation { cmd: u8, code: ResponseCode },
    InvalidData { err: pmbus::Error },
    BadFirmwareRevRead { code: ResponseCode },
    BadFirmwareRev { index: u8 },
    BadFirmwareRevLength,
    BadModelNumberRead { code: ResponseCode },
    BadMfrIdRead { code: ResponseCode },
    UnsupportedCommand { cmd: u8 },
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

impl Mwocp67 {
    pub fn new(device: &I2cDevice, index: u8) -> Self {
        Mwocp67 {
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
            3 => pmbus_read!(self.device, READ_TEMP_CLIP_P)?.get()?,
            4 => pmbus_read!(self.device, READ_TEMP_CLIP_N)?.get()?,
            _ => {
                return Err(Error::InvalidData {
                    err: pmbus::Error::InvalidCode,
                });
            }
        };
        Ok(r)
    }

    pub fn read_speed(&self) -> Result<Rpm, Error> {
        Ok(pmbus_read!(self.device, READ_FAN_SPEED_1)?.get()?)
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
            Operation::IoutOcFaultLimit => PmbusValue::from(
                pmbus_read!(self.device, IOUT_OC_FAULT_LIMIT)?.get()?,
            ),
            Operation::IoutOcWarnLimit => PmbusValue::from(
                pmbus_read!(self.device, IOUT_OC_WARN_LIMIT)?.get()?,
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
                PmbusValue::from(pmbus_read!(self.device, READ_VCAP)?.get()?)
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
            Operation::ReadTempClipP => PmbusValue::from(
                pmbus_read!(self.device, READ_TEMP_CLIP_P)?.get()?,
            ),
            Operation::ReadTempClipN => PmbusValue::from(
                pmbus_read!(self.device, READ_TEMP_CLIP_N)?.get()?,
            ),
            Operation::ReadFanSpeed1 => PmbusValue::from(
                pmbus_read!(self.device, READ_FAN_SPEED_1)?.get()?,
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
            Operation::ReadFanSpeed2
            | Operation::FanCommand2
            | Operation::OtWarnLimit => {
                return Err(Error::UnsupportedCommand { cmd: op as u8 });
            }
        };

        Ok(val)
    }

    /// Will return true if the device is present and valid -- false otherwise
    pub fn present(&self) -> bool {
        Mwocp67::validate(&self.device).unwrap_or_default()
    }

    pub fn power_good(&self) -> Result<bool, Error> {
        use commands::mwocp67::STATUS_WORD::*;

        let status = pmbus_read!(self.device, STATUS_WORD)?;
        Ok(status.get_power_good_status() == Some(PowerGoodStatus::PowerGood))
    }

    ///
    /// Returns the firmware revision of the primary MCU (AC input side).
    ///
    pub fn firmware_revision(&self) -> Result<FirmwareRev, Error> {
        const REVISION_LEN: usize = 14;

        let mut data = [0u8; REVISION_LEN];
        let expected = b"XXXX-YYYY-0000";

        let len = self
            .device
            .read_block(CommandCode::MFR_REVISION as u8, &mut data)
            .map_err(|code| Error::BadFirmwareRevRead { code })?;

        //
        // Per ACAN-157, we are expecting this to be of the format:
        //
        //    XXXX-YYYY-0000
        //
        // Where XXXX is the firmware revision on the primary MCU (AC input
        // side) and YYYY is the firmware revision on the secondary MCU (DC
        // output side).  We aren't going to be rigid about the format of
        // either revision, but we will be rigid about the rest of the format.
        //
        if len != REVISION_LEN {
            return Err(Error::BadFirmwareRevLength);
        }

        for index in 0..len {
            if expected[index] == b'X' || expected[index] == b'Y' {
                continue;
            }

            if data[index] != expected[index] {
                return Err(Error::BadFirmwareRev { index: index as u8 });
            }
        }

        //
        // Return the primary MCU version
        //
        Ok(FirmwareRev([data[0], data[1], data[2], data[3]]))
    }

    ///
    /// Returns the serial number of the PSU.
    ///
    pub fn serial_number(&self) -> Result<SerialNumber, Error> {
        let mut serial = SerialNumber::default();

        let _ = self
            .device
            .read_block(CommandCode::MFR_SERIAL as u8, &mut serial.0)
            .map_err(|code| Error::BadFirmwareRevRead { code })?;

        Ok(serial)
    }

    ///
    /// Returns the manufacturer model number of the PSU.
    ///
    pub fn model_number(&self) -> Result<ModelNumber, Error> {
        let mut model = ModelNumber::default();
        let _ = self
            .device
            .read_block(CommandCode::MFR_MODEL as u8, &mut model.0)
            .map_err(|code| Error::BadModelNumberRead { code })?;
        Ok(model)
    }

    ///
    /// Returns the manufacturer ID of the PSU.
    ///
    pub fn mfr_id(&self) -> Result<MfrId, Error> {
        let mut id = MfrId::default();
        let _ = self
            .device
            .read_block(CommandCode::MFR_ID as u8, &mut id.0)
            .map_err(|code| Error::BadMfrIdRead { code })?;
        Ok(id)
    }

    pub fn status_word(&self) -> Result<STATUS_WORD::CommandData, Error> {
        // ACAN-157 doesn't specify what page this is on.
        // Assume it's on page 0, as it is on the better-documented mwocp68.
        pmbus_rail_read!(self.device, 0, STATUS_WORD)
    }

    pub fn status_iout(&self) -> Result<STATUS_IOUT::CommandData, Error> {
        // ACAN-157 doesn't specify what page this is on.
        // Assume it's on page 0, as it is on the better-documented mwocp68.
        pmbus_rail_read!(self.device, 0, STATUS_IOUT)
    }

    pub fn status_vout(&self) -> Result<STATUS_VOUT::CommandData, Error> {
        // ACAN-157 doesn't specify what page this is on.
        // Assume it's on page 0, as it is on the better-documented mwocp68.
        pmbus_rail_read!(self.device, 0, STATUS_VOUT)
    }

    pub fn status_input(&self) -> Result<STATUS_INPUT::CommandData, Error> {
        pmbus_read!(self.device, STATUS_INPUT)
    }

    pub fn status_cml(&self) -> Result<STATUS_CML::CommandData, Error> {
        pmbus_read!(self.device, STATUS_CML)
    }

    pub fn status_temperature(
        &self,
    ) -> Result<STATUS_TEMPERATURE::CommandData, Error> {
        pmbus_read!(self.device, STATUS_TEMPERATURE)
    }

    pub fn status_mfr_specific(
        &self,
    ) -> Result<STATUS_MFR_SPECIFIC::CommandData, Error> {
        pmbus_read!(self.device, STATUS_MFR_SPECIFIC)
    }

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
    }

    pub fn rail_name(psu_label: char) -> FixedString<8> {
        // This is a little silly, but it stops us from having to 6 separate
        // instances of the string "V50_MAIN_PSU" in the binary...
        let mut name = *b"V50_MAIN_PSUx";
        name[7] = psu_label as u8;
        FixedString::try_from_utf8(&name[..]).unwrap_lite()
    }
}

impl Validate<Error> for Mwocp67 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = b"MWOCP67-5500-B-RM";
        pmbus_validate(device, CommandCode::MFR_MODEL, expected)
            .map_err(Into::into)
    }
}

impl VoltageSensor<Error> for Mwocp67 {
    fn read_vout(&self) -> Result<Volts, Error> {
        self.set_rail()?;
        let vout = pmbus_read!(self.device, READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}

impl CurrentSensor<Error> for Mwocp67 {
    fn read_iout(&self) -> Result<Amperes, Error> {
        self.set_rail()?;
        let iout = pmbus_read!(self.device, READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}

impl InputVoltageSensor<Error> for Mwocp67 {
    fn read_vin(&self) -> Result<Volts, Error> {
        self.set_rail()?;
        let vin = pmbus_read!(self.device, READ_VIN)?;
        Ok(Volts(vin.get()?.0))
    }
}

impl InputCurrentSensor<Error> for Mwocp67 {
    fn read_iin(&self) -> Result<Amperes, Error> {
        self.set_rail()?;
        let iin = pmbus_read!(self.device, READ_IIN)?;
        Ok(Amperes(iin.get()?.0))
    }
}
