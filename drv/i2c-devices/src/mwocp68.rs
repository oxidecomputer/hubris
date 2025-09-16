// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! MWOCP68-3600 Murata power shelf

use crate::{
    pmbus_validate, BadValidation, CurrentSensor, InputCurrentSensor,
    InputVoltageSensor, Validate, VoltageSensor,
};
use core::cell::Cell;
use drv_i2c_api::*;
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

#[derive(Copy, Clone, PartialEq)]
pub struct FirmwareRev(pub [u8; 4]);

#[derive(Copy, Clone, PartialEq, Default)]
pub struct SerialNumber(pub [u8; SERIAL_LEN]);

const SERIAL_LEN: usize = 12;
const REVISION_LEN: usize = 14;

//
// The boot loader command -- sent via BOOT_LOADER_CMD -- is unfortunately odd
// in that its command code is overloaded with BOOT_LOADER_STATUS.  (That is,
// a read to the command code is BOOT_LOADER_STATUS, a write is
// BOOT_LOADER_CMD.)  This is behavior that the PMBus crate didn't necessarily
// envision, so it can't necessarily help us out; we define the single-byte
// payload codes here rather than declaratively in the PMBus crate.
//
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BootLoaderCommand {
    ClearStatus = 0x00,
    RestartProgramming = 0x01,
    BootPrimary = 0x12,
    BootSecondary = 0x02,
    BootPSUFirmware = 0x03,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    BadRead {
        cmd: u8,
        code: ResponseCode,
    },
    BadWrite {
        cmd: u8,
        code: ResponseCode,
    },
    BadData {
        cmd: u8,
    },
    BadValidation {
        cmd: u8,
        code: ResponseCode,
    },
    InvalidData {
        err: pmbus::Error,
    },
    BadFirmwareRevRead {
        code: ResponseCode,
    },
    BadFirmwareRev {
        index: u8,
    },
    BadFirmwareRevLength,
    UpdateInBootLoader,
    UpdateNotInBootLoader,
    UpdateAlreadySuccessful,
    BadBootLoaderStatus {
        data: u8,
    },
    BadBootLoaderCommand {
        cmd: BootLoaderCommand,
        code: ResponseCode,
    },
    ChecksumNotSuccessful,
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

///
/// Defines the state of the firmware update.  Once `UpdateSuccessful`
/// has been returned, the update is complete.
///
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum UpdateState {
    /// The boot loader key has been written
    WroteBootLoaderKey,

    /// The product key has been written
    WroteProductKey,

    /// The boot loader has been booted
    BootedBootLoader,

    /// Programming of firmware has been indicated to have started
    StartedProgramming,

    /// A block has been written; the next offset is at [`offset`], and the
    /// running checksum is in [`checksum`]
    WroteBlock { offset: usize, checksum: u64 },

    /// The last block has been written; the checksum is in [`checksum`]
    WroteLastBlock { checksum: u64 },

    /// The checksum has been sent for verification
    SentChecksum,

    /// The checksum has been verified
    VerifiedChecksum,

    /// The PSU has been rebooted
    RebootedPSU,

    /// The entire update is complete and successful
    UpdateSuccessful,
}

impl UpdateState {
    ///
    /// Return the milliseconds of delay associated with the current state.
    /// Note that some of these values differ slightly from Murata's "PSU
    /// Firmware Update Process" document in that they reflect revised
    /// guidance from Murata.
    ///
    fn delay_ms(&self) -> u64 {
        match self {
            Self::WroteBootLoaderKey => 3_000,
            Self::WroteProductKey => 3_000,
            Self::BootedBootLoader => 1_000,
            Self::StartedProgramming => 2_000,
            Self::WroteBlock { .. } | Self::WroteLastBlock { .. } => 100,
            Self::SentChecksum => 2_000,
            Self::VerifiedChecksum => 4_000,
            Self::RebootedPSU => 5_000,
            Self::UpdateSuccessful => 0,
        }
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

    /// Will return true if the device is present and valid -- false otherwise
    pub fn present(&self) -> bool {
        Mwocp68::validate(&self.device).unwrap_or_default()
    }

    pub fn power_good(&self) -> Result<bool, Error> {
        use commands::mwocp68::STATUS_WORD::*;

        let status = pmbus_read!(self.device, STATUS_WORD)?;
        Ok(status.get_power_good_status() == Some(PowerGoodStatus::PowerGood))
    }

    ///
    /// Returns the firmware revision of the primary MCU (AC input side).
    ///
    pub fn firmware_revision(&self) -> Result<FirmwareRev, Error> {
        let mut data = [0u8; REVISION_LEN];
        let expected = b"XXXX-YYYY-0000";

        let len = self
            .device
            .read_block(CommandCode::MFR_REVISION as u8, &mut data)
            .map_err(|code| Error::BadFirmwareRevRead { code })?;

        //
        // Per ACAN-114, we are expecting this to be of the format:
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

    fn get_boot_loader_status(
        &self,
    ) -> Result<BOOT_LOADER_STATUS::CommandData, Error> {
        use pmbus::commands::mwocp68::CommandCode;
        let cmd = CommandCode::BOOT_LOADER_STATUS as u8;
        let mut data = [0u8];

        match self.device.read_block(cmd, &mut data) {
            Ok(1) => Ok(()),
            Ok(len) => Err(Error::BadBootLoaderStatus { data: len as u8 }),
            Err(code) => Err(Error::BadRead { cmd, code }),
        }?;

        match BOOT_LOADER_STATUS::CommandData::from_slice(&data[0..]) {
            Some(status) => Ok(status),
            None => Err(Error::BadBootLoaderStatus { data: data[0] }),
        }
    }

    fn get_boot_loader_mode(&self) -> Result<BOOT_LOADER_STATUS::Mode, Error> {
        //
        // This unwrap is safe because the boot loader mode is a single bit.
        //
        Ok(self.get_boot_loader_status()?.get_mode().unwrap())
    }

    fn boot_loader_command(&self, cmd: BootLoaderCommand) -> Result<(), Error> {
        use pmbus::commands::mwocp68::CommandCode;

        //
        // The great unfortunateness: BOOT_LOADER_STATUS is overloaded to
        // be BOOT_LOADER_CMD on a write.
        //
        let data = [CommandCode::BOOT_LOADER_STATUS as u8, 1, cmd as u8];

        self.device
            .write(&data)
            .map_err(|code| Error::BadBootLoaderCommand { cmd, code })?;

        Ok(())
    }

    ///
    /// Perform a firmware update, implementating the procedure contained
    /// within Murata's "PSU Firmware Update Process" document.  Note that
    /// this function must be called initially with a state of `None`; it will
    /// return either an error, or the next state in the update process,
    /// along with a specified delay in milliseconds.  It is up to the caller
    /// to assure that the returned delay has been observed before calling
    /// back into continue the update.
    ///
    pub fn update(
        &self,
        state: Option<UpdateState>,
        payload: &[u8],
    ) -> Result<(UpdateState, u64), Error> {
        use pmbus::commands::mwocp68::CommandCode;
        use BOOT_LOADER_STATUS::Mode;

        let write_boot_loader_key = || -> Result<UpdateState, Error> {
            const MWOCP68_BOOT_LOADER_KEY: &[u8] = b"InVe";
            let mut data = [0u8; MWOCP68_BOOT_LOADER_KEY.len() + 2];

            data[0] = CommandCode::BOOT_LOADER_KEY as u8;
            data[1] = MWOCP68_BOOT_LOADER_KEY.len() as u8;
            data[2..].copy_from_slice(MWOCP68_BOOT_LOADER_KEY);

            self.device
                .write(&data)
                .map_err(|code| Error::BadWrite { cmd: data[0], code })?;

            Ok(UpdateState::WroteBootLoaderKey)
        };

        let write_product_key = || -> Result<UpdateState, Error> {
            const MWOCP68_PRODUCT_KEY: &[u8] = b"M5813-0000000000";
            let mut data = [0u8; MWOCP68_PRODUCT_KEY.len() + 1];

            data[0] = CommandCode::BOOT_LOADER_PRODUCT_KEY as u8;
            data[1..].copy_from_slice(MWOCP68_PRODUCT_KEY);

            self.device
                .write(&data)
                .map_err(|code| Error::BadWrite { cmd: data[0], code })?;

            Ok(UpdateState::WroteProductKey)
        };

        let boot_boot_loader = || -> Result<UpdateState, Error> {
            self.boot_loader_command(BootLoaderCommand::BootPrimary)?;
            Ok(UpdateState::BootedBootLoader)
        };

        let start_programming = || -> Result<UpdateState, Error> {
            self.boot_loader_command(BootLoaderCommand::RestartProgramming)?;
            Ok(UpdateState::StartedProgramming)
        };

        let write_block = || -> Result<UpdateState, Error> {
            const BLOCK_LEN: usize = 32;

            let (mut offset, mut checksum) = match state {
                Some(UpdateState::WroteBlock { offset, checksum }) => {
                    (offset, checksum)
                }
                Some(UpdateState::StartedProgramming) => (0, 0),
                _ => panic!(),
            };

            let mut data = [0u8; BLOCK_LEN + 1];
            data[0] = CommandCode::BOOT_LOADER_MEMORY_BLOCK as u8;
            data[1..].copy_from_slice(&payload[offset..offset + BLOCK_LEN]);

            self.device
                .write(&data)
                .map_err(|code| Error::BadWrite { cmd: data[0], code })?;

            checksum = data[1..]
                .iter()
                .fold(checksum, |c, &d| c.wrapping_add(d.into()));
            offset += BLOCK_LEN;

            if offset >= payload.len() {
                Ok(UpdateState::WroteLastBlock { checksum })
            } else {
                Ok(UpdateState::WroteBlock { offset, checksum })
            }
        };

        let send_checksum = || -> Result<UpdateState, Error> {
            let Some(UpdateState::WroteLastBlock { checksum }) = state else {
                panic!();
            };

            let data = [
                CommandCode::IMAGE_CHECKSUM as u8,
                2,
                (checksum & 0xff) as u8,
                ((checksum >> 8) & 0xff) as u8,
            ];

            self.device
                .write(&data)
                .map_err(|code| Error::BadWrite { cmd: data[0], code })?;

            Ok(UpdateState::SentChecksum)
        };

        let verify_checksum = || -> Result<UpdateState, Error> {
            use BOOT_LOADER_STATUS::ChecksumSuccessful;

            let status = self.get_boot_loader_status()?;

            match status.get_checksum_successful() {
                Some(ChecksumSuccessful::Successful) => {
                    Ok(UpdateState::VerifiedChecksum)
                }
                Some(ChecksumSuccessful::NotSuccessful) | None => {
                    Err(Error::ChecksumNotSuccessful)
                }
            }
        };

        let reboot_psu = || -> Result<UpdateState, Error> {
            self.boot_loader_command(BootLoaderCommand::BootPSUFirmware)?;
            Ok(UpdateState::RebootedPSU)
        };

        let verify_success = || -> Result<UpdateState, Error> {
            Ok(UpdateState::UpdateSuccessful)
        };

        //
        // We want to confirm that our boot loader is in the state that
        // we think it should be in.  On the one hand, this will fail in
        // a non-totally-unreasonable fashion if we don't check this -- but
        // we have an opportunity to assert our in-device state and fail
        // cleanly if it doesn't match, and it feels like we should take it.
        //
        let expected = match state {
            None
            | Some(UpdateState::WroteBootLoaderKey)
            | Some(UpdateState::WroteProductKey)
            | Some(UpdateState::RebootedPSU) => Mode::NotBootLoader,

            Some(UpdateState::BootedBootLoader)
            | Some(UpdateState::StartedProgramming)
            | Some(UpdateState::WroteBlock { .. })
            | Some(UpdateState::WroteLastBlock { .. })
            | Some(UpdateState::SentChecksum)
            | Some(UpdateState::VerifiedChecksum) => Mode::BootLoader,

            Some(UpdateState::UpdateSuccessful) => {
                return Err(Error::UpdateAlreadySuccessful);
            }
        };

        if self.get_boot_loader_mode()? != expected {
            return Err(match expected {
                Mode::BootLoader => Error::UpdateNotInBootLoader,
                Mode::NotBootLoader => Error::UpdateInBootLoader,
            });
        }

        let next = match state {
            None => write_boot_loader_key()?,
            Some(UpdateState::WroteBootLoaderKey) => write_product_key()?,
            Some(UpdateState::WroteProductKey) => boot_boot_loader()?,
            Some(UpdateState::BootedBootLoader) => start_programming()?,
            Some(UpdateState::StartedProgramming)
            | Some(UpdateState::WroteBlock { .. }) => write_block()?,
            Some(UpdateState::WroteLastBlock { .. }) => send_checksum()?,
            Some(UpdateState::SentChecksum) => verify_checksum()?,
            Some(UpdateState::VerifiedChecksum) => reboot_psu()?,
            Some(UpdateState::RebootedPSU) => verify_success()?,
            Some(UpdateState::UpdateSuccessful) => panic!(),
        };

        Ok((next, next.delay_ms()))
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

impl crate::PmbusVpd for Mwocp68 {
    const HAS_MFR_DATE: bool = true;
    const HAS_MFR_LOCATION: bool = true;
    const HAS_MFR_SERIAL: bool = true;
    const HAS_IC_DEVICE_IDENTITY: bool = true;
}
