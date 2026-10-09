// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! MWOCP67-5500 Murata power shelf

use crate::mwocp6x::{
    Error, FIRMWARE_REVISION_LEN, FirmwareImage, FirmwareRev, MfrId,
    ModelNumber, SerialNumber, parse_firmware_revision,
};
use crate::{
    CurrentSensor, InputCurrentSensor, InputVoltageSensor, Validate,
    VoltageSensor, pmbus_validate,
};
use core::cell::Cell;
use drv_i2c_api::*;
use pmbus::commands::CommandCode;
use pmbus::commands::mwocp67::*;
use pmbus::units::{Celsius, Rpm};
use pmbus::*;
use task_power_api::PmbusValue;
use userlib::units::{Amperes, Volts};

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum UpdateState {
    EnteredUploadMode,
    WroteBlock { block_index: usize },
    WroteLastBlock,
    UpdateSuccessful,
}

impl UpdateState {
    /// Return how many milliseconds to wait after the `update` function returns
    /// this state.
    pub(crate) fn delay_ms(&self) -> u64 {
        match self {
            // Wait for PSU to erase program memory
            Self::EnteredUploadMode => 5_000,
            // Wait for PSU to write program memory
            Self::WroteBlock { .. } => 10,
            // Wait for PSU to verify checksum
            Self::WroteLastBlock => 12_000,
            // If we don't delay before checking the revision, the PSU will
            // report the old firmware's revision and we will think that the
            // update failed. Presumably this delay gives the PSU time to reboot
            // into the new firmware.
            Self::UpdateSuccessful => 1_000,
        }
    }
}

/// A more convenient representation of the MFR_FWUPLOAD_STATUS register.
struct UploadStatus {
    command_format_mismatch: bool,
    image_unsupported: bool,
    image_corrupt: bool,
    #[allow(unused)]
    full_image_not_received_yet: bool,
    full_image_received_successfully: bool,
}

pub struct Mwocp67 {
    device: I2cDevice,

    /// The index represents PMBus rail when reading voltage / current,
    /// the sensor index when reading temperature (0-4), and is ignored when
    /// reading the speed of the single fan.
    index: u8,

    mode: Cell<Option<pmbus::VOutModeCommandData>>,
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

    pub fn read_fan_speed(&self) -> Result<Rpm, Error> {
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
            Operation::Operation => {
                let (val, width) = pmbus_read!(self.device, OPERATION)?.raw();
                assert_eq!(width.0, 8);
                PmbusValue::Raw8(val as u8)
            }
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

    /// Enables or disables the power supply output via the PMBus `OPERATION`
    /// command. This affects only the main rail, not the aux rail.
    pub fn set_enabled(&self, enable: bool) -> Result<(), Error> {
        // The datasheet doesn't mention any fields besides the on_off_state
        // bit, but let's do a read-modify-write to be safe.
        let mut data = pmbus_read!(self.device, OPERATION)?;
        data.set_on_off_state(if enable {
            OPERATION::OnOffState::On
        } else {
            OPERATION::OnOffState::Off
        });
        pmbus_write!(self.device, OPERATION, data)
    }

    /// Reports whether the power supply output is currently commanded on,
    /// according to the on/off state in the PMBus `OPERATION` command.
    ///
    /// Note that the return value is not affected by fault conditions. If the
    /// output was commanded on but was then automatically latched off due to a
    /// fault, this function will still return true.
    pub fn is_enabled(&self) -> Result<bool, Error> {
        let data = pmbus_read!(self.device, OPERATION)?;
        let state = data.get_on_off_state().ok_or(Error::BadData {
            cmd: OPERATION::CommandData::code(),
        })?;
        Ok(state == OPERATION::OnOffState::On)
    }

    /// Clears faults and status registers, allowing the PSU to resume operation
    /// if it was latched off due to a fault.
    pub fn clear_faults_and_latch(&self) -> Result<(), Error> {
        let mut data = pmbus_read!(self.device, MB_PSU_SETTING)?;
        data.set_clear_faults(MB_PSU_SETTING::ClearFaults::Clear);
        pmbus_write!(self.device, MB_PSU_SETTING, data)
    }

    ///
    /// Returns the firmware revision of the primary and secondary MCUs.
    ///
    pub fn firmware_revision(&self) -> Result<FirmwareRev, Error> {
        let mut data = [0u8; FIRMWARE_REVISION_LEN];

        let len = self
            .device
            .read_block(CommandCode::MFR_REVISION as u8, &mut data)
            .map_err(|code| Error::BadFirmwareRevRead { code })?;

        if len != FIRMWARE_REVISION_LEN {
            return Err(Error::BadFirmwareRevLength);
        }

        parse_firmware_revision(&data)
            .map_err(|index| Error::BadFirmwareRev { index })
    }

    ///
    /// Returns the serial number of the PSU.
    ///
    pub fn serial_number(&self) -> Result<SerialNumber, Error> {
        let mut serial = SerialNumber::default();

        let _ = self
            .device
            .read_block(CommandCode::MFR_SERIAL as u8, &mut serial.0)
            .map_err(|code| Error::BadSerialNumberRead { code })?;

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

    fn enter_upload_mode(&self) -> Result<(), Error> {
        let mut data = MFR_FWUPLOAD_MODE::CommandData(0);
        data.set_enter_or_exit(MFR_FWUPLOAD_MODE::EnterOrExit::Enter);
        pmbus_write!(self.device, MFR_FWUPLOAD_MODE, data)
    }

    fn exit_upload_mode(&self) -> Result<(), Error> {
        let mut data = MFR_FWUPLOAD_MODE::CommandData(0);
        data.set_enter_or_exit(MFR_FWUPLOAD_MODE::EnterOrExit::Exit);
        pmbus_write!(self.device, MFR_FWUPLOAD_MODE, data)
    }

    fn get_upload_mode(&self) -> Result<MFR_FWUPLOAD_MODE::EnterOrExit, Error> {
        let data = pmbus_read!(self.device, MFR_FWUPLOAD_MODE)?;
        data.get_enter_or_exit().ok_or(Error::BadData {
            cmd: MFR_FWUPLOAD_MODE::CommandData::code(),
        })
    }

    fn get_upload_status(&self) -> Result<UploadStatus, Error> {
        let register = pmbus_read!(self.device, MFR_FWUPLOAD_STATUS)?;
        let bad_data = Error::BadData {
            cmd: MFR_FWUPLOAD_STATUS::CommandData::code(),
        };
        let command_format_mismatch =
            register.get_command_format_mismatch().ok_or(bad_data)?
                == MFR_FWUPLOAD_STATUS::CommandFormatMismatch::Mismatch;
        let image_unsupported =
            register.get_image_unsupported().ok_or(bad_data)?
                == MFR_FWUPLOAD_STATUS::ImageUnsupported::Unsupported;
        let image_corrupt = register.get_image_corrupt().ok_or(bad_data)?
            == MFR_FWUPLOAD_STATUS::ImageCorrupt::Yes;
        let full_image_not_received_yet =
            register.get_full_image_not_received_yet().ok_or(bad_data)?
                == MFR_FWUPLOAD_STATUS::FullImageNotReceivedYet::NotDone;
        let full_image_received_successfully = register
            .get_full_image_received_successfully()
            .ok_or(bad_data)?
            == MFR_FWUPLOAD_STATUS::FullImageReceivedSuccessfully::Yes;

        Ok(UploadStatus {
            command_format_mismatch,
            image_unsupported,
            image_corrupt,
            full_image_not_received_yet,
            full_image_received_successfully,
        })
    }

    /// Perform a firmware update, implementing the procedure contained within
    /// Murata's ACAN-157 document. Note that this function must be called
    /// initially with a state of `None`; it will return either an error, or the
    /// next state in the update process, along with a specified delay in
    /// milliseconds. It is up to the caller to assure that the returned delay
    /// has been observed before calling back in to continue the update.
    pub fn update(
        &self,
        state: Option<UpdateState>,
        firmware: FirmwareImage,
    ) -> Result<(UpdateState, u64), Error> {
        // All the interesting stuff happens in `update_impl()`. This wrapper just
        // ensures that we'll (attempt to) take the PSU out of upload mode after
        // an update fails for any reason. This will allow the PSU to function
        // normally again, running the original firmware from before the update.
        let result = self.update_impl(state, firmware);
        if result.is_err() {
            let _ = self.exit_upload_mode();
        }
        result
    }

    fn update_impl(
        &self,
        state: Option<UpdateState>,
        firmware: FirmwareImage,
    ) -> Result<(UpdateState, u64), Error> {
        // We should have already entered upload mode the first time `update()`
        // was called, so if the PSU is not still in upload mode, then something
        // weird happened. (But note that we don't need to report an error if
        // the PSU was already in upload mode when `update()` was called for the
        // first time. The docs say that you can re-enter upload mode at any
        // time to restart the update process.)
        let ensure_in_upload_mode = || -> Result<(), Error> {
            if self.get_upload_mode()? != MFR_FWUPLOAD_MODE::EnterOrExit::Enter
            {
                // Ideally the error would be named "NotInUploadMode", but we need
                // to share an error enum with the mwocp68, and this is basically
                // the same situation as the mwocp68's "NotInBootloader" error.
                Err(Error::UpdateNotInBootLoader)
            } else {
                Ok(())
            }
        };

        // Sends one 32-byte block of the payload to the PSU
        let write_block = |block_index: usize| -> Result<UpdateState, Error> {
            const BLOCK_LEN: usize = 32;
            let mut blocks = firmware.payload.chunks(BLOCK_LEN);
            let num_blocks = blocks.len();
            let block = blocks
                .nth(block_index)
                .ok_or(Error::UpdateBlockOutOfBounds)?;

            // All the binaries that we've seen so far have had lengths that are
            // a multiple of 32 bytes, so they can be evenly divided into
            // blocks. It's unclear what we should do if that's ever not true,
            // but padding the end of the final block with 0xFF seems like a
            // reasonable choice.
            let mut data = [0xFFu8; BLOCK_LEN + 2];
            data[0] = pmbus::commands::mwocp67::CommandCode::MFR_FWUPLOAD as u8;
            data[1] = BLOCK_LEN as u8;
            data[2..2 + block.len()].copy_from_slice(block);

            self.device
                .write(&data)
                .map_err(|code| Error::BadWrite { cmd: data[0], code })?;

            if block_index == num_blocks - 1 {
                Ok(UpdateState::WroteLastBlock)
            } else {
                Ok(UpdateState::WroteBlock { block_index })
            }
        };

        let next = match state {
            None => {
                // Note that we don't need to specify which MCU we'll be
                // updating. Unlike the mwocp68, the mwocp67 can automatically
                // detect whether a firmware image is intended for the primary
                // or secondary MCU.
                self.enter_upload_mode()?;
                UpdateState::EnteredUploadMode
            }
            Some(UpdateState::EnteredUploadMode) => {
                ensure_in_upload_mode()?;
                write_block(0)?
            }
            Some(UpdateState::WroteBlock { block_index }) => {
                ensure_in_upload_mode()?;

                let status = self.get_upload_status()?;
                if status.command_format_mismatch {
                    return Err(Error::UpdateCommandFormatMismatch);
                }
                if status.image_unsupported {
                    return Err(Error::UpdateImageUnsupported);
                }
                write_block(block_index + 1)?
            }
            Some(UpdateState::WroteLastBlock) => {
                ensure_in_upload_mode()?;
                let status = self.get_upload_status()?;
                if !status.full_image_received_successfully {
                    if status.image_corrupt {
                        return Err(Error::ChecksumNotSuccessful);
                    } else if status.command_format_mismatch {
                        return Err(Error::UpdateCommandFormatMismatch);
                    } else if status.image_unsupported {
                        return Err(Error::UpdateImageUnsupported);
                    } else {
                        return Err(Error::UnknownUpdateError);
                    }
                }
                self.exit_upload_mode()?;
                // The PSU will now reboot into the new firmware
                UpdateState::UpdateSuccessful
            }
            Some(UpdateState::UpdateSuccessful) => {
                return Err(Error::UpdateAlreadySuccessful);
            }
        };
        Ok((next, next.delay_ms()))
    }

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
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
