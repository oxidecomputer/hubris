// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the BMR491 IBC

use core::cell::Cell;

use crate::{
    pmbus_validate, BadValidation, CurrentSensor, TempSensor, Validate,
    VoltageSensor,
};
use drv_i2c_api::*;
use pmbus::commands::*;
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::units::*;

#[derive(Copy, Clone, PartialEq)]
pub enum Trace {
    None,
    MitigationFailed(MitigationFailureKind),
    MitigationApplied(MitigationAction),
}

ringbuf!(Trace, 8, Trace::None);

pub struct Bmr491 {
    device: I2cDevice,
    mode: Cell<Option<pmbus::VOutModeCommandData>>,
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

/// Specifies the class of external (to the BMR491) input voltage protection
/// present in a system.
///
/// This is given to the mitigation routine, to avoid having a naked bool
/// parameter to explain at callsites.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExternalInputVoltageProtection {
    /// Something upstream of the BMR491 protects us against voltages below 40
    /// V.
    CutoffAt40V,
    /// Upstream voltages may sag below 40 V.
    CutoffBelow40V,
}

/// The result of applying a mitigation, for reporting.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MitigationResult {
    /// The action taken in the end (successfully).
    pub action_taken: MitigationAction,
    /// Number of failures that occurred (may be zero).
    pub failures: u32,
    /// If failures is nonzero, this is the last one.
    pub last_failure: Option<MitigationFailureKind>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MitigationAction {
    /// We inspected the BMR491 and its revision code suggests that it's immune
    /// to the issue.
    NotNecessaryOnThisRevision,
    /// The BMR491's registers show that the mitigation was already applied.
    AlreadyApplied,
    /// We have applied the mitigation in response to this request.
    NewlyApplied,
}

/// Information about a failure to apply a mitigation.
///
/// This throws away the specific PMBus error codes because they've already
/// thrown away most of the useful information, due to the I2C API's flattening
/// of errors. This should still be useful.
#[derive(Copy, Clone, Debug, Eq, PartialEq, microcbor::Encode)]
pub enum MitigationFailureKind {
    #[cbor(rename = "RevRead")]
    RevisionReadFailed,
    #[cbor(rename = "StateRead")]
    StateReadFailed,
    #[cbor(rename = "Update")]
    UpdateFailed,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MitigationFailure {
    pub last_cause: MitigationFailureKind,
    pub retries: u32,
}

impl Bmr491 {
    pub fn new(device: &I2cDevice, _rail: u8) -> Self {
        Bmr491 {
            device: *device,
            mode: Cell::new(None),
        }
    }

    /// Applies a firmware-configuration workaround for the component tolerance
    /// issue described in (unfortunately confidential) Flex document
    /// RMA2402311, where over-eager input undervoltage sensing can result in
    /// output glitches.
    ///
    /// The theory of operation of the mitigation is as follows:
    ///
    /// - An input filter in the R1C revision was built incorrectly and allows
    ///   brief negative transients to reach the controller chip.
    ///
    /// - To stop the controller chip from reacting to these transients, Flex
    ///   suggests disabling automatic response to input undervoltage events.
    ///
    /// - To retain some degree of brownout protection, they recommend instead
    ///   enabling the _output_ undervoltage monitor.
    ///
    /// This function will check to see if the mitigation is already applied, or
    /// if the device reports an unaffected revision. In either case, it will
    /// leave the settings untouched.
    ///
    /// The output undervoltage monitor will be configured _only_ on request
    /// (when the `external_protection` parameter is `CutoffBelow40V`). This is
    /// because most of our existing machines already have undervoltage
    /// protection on the input that will effectively override the detector.
    ///
    /// The details and rationale are in RFD630 for internal readers, but I've
    /// attempted to restate things here for external readers.
    pub fn apply_mitigation_for_rma2402311(
        &self,
        external_protection: ExternalInputVoltageProtection,
    ) -> Result<MitigationResult, MitigationFailure> {
        // Somewhat arbitrarily selected retry count -- we don't expect retries
        // to be required, but retrying seems better than the alternatives when
        // not applying the mitigation could impact server stability.
        const RETRIES: u32 = 3;

        let mut failures = 0;
        let mut last_cause = None;
        while failures < RETRIES {
            let r =
                self.apply_mitigation_for_rma2402311_once(external_protection);
            match r {
                Ok(action_taken) => {
                    ringbuf_entry!(Trace::MitigationApplied(action_taken));
                    return Ok(MitigationResult {
                        action_taken,
                        failures,
                        last_failure: last_cause,
                    });
                }
                Err(e) => {
                    ringbuf_entry!(Trace::MitigationFailed(e));
                    failures += 1;
                    last_cause = Some(e);
                }
            }
        }

        Err(MitigationFailure {
            last_cause: last_cause.unwrap(),
            retries: failures,
        })
    }

    fn apply_mitigation_for_rma2402311_once(
        &self,
        external_protection: ExternalInputVoltageProtection,
    ) -> Result<MitigationAction, MitigationFailureKind> {
        use pmbus::commands::bmr491::{
            CommandCode, MAX_DUTY, VIN_OFF, VOUT_UV_FAULT_LIMIT,
        };
        // The length of the revision buffer is specified by the BMR491
        // Technical Specification in the "PMBus Command Summary And Factory
        // Default Values" table.
        let mut rev = [0u8; 12];
        self.device
            .read_block(CommandCode::MFR_REVISION as u8, &mut rev)
            .map_err(|_| MitigationFailureKind::RevisionReadFailed)?;

        // Currently, the defect is known to exist in R1C parts, and may have
        // been present in earlier parts (it's not clear that we have any). It
        // is supposed to be fixed in R1D, and in the event that an R1E is
        // released, it will _probably_ remain fixed.
        //
        // But since we don't know that for sure, currently we're treating only
        // the R1D revision as fixed. Applying the mitigation to fixed future
        // IBCs should not be destructive, so we can fix the firmware when that
        // day comes.
        if rev.starts_with(b"R1D ") {
            // Assume the mitigation is unnecessary.
            return Ok(MitigationAction::NotNecessaryOnThisRevision);
        }

        // Read out the affected registers to see if we've already applied the
        // mitigation.
        let current_vin_off = pmbus_read!(self.device, VIN_OFF)
            .map_err(|_| MitigationFailureKind::StateReadFailed)?;
        let basic_mitigation_ok = current_vin_off.0 == 0;

        // This is 95%.
        const EXPECTED_MAX_DUTY: u16 = 0xEAF8;
        // 11 V specified in units of 1/8 V.
        const EXPECTED_UV_FAULT_LIMIT: u16 = 11 * 8;
        // See the documentation for VOUT_UV_FAULT_RESPONSE for the bit fields.
        const EXPECTED_UV_FAULT_RESPONSE: u8 = 0x80;

        let vout_detect_ok = match external_protection {
            ExternalInputVoltageProtection::CutoffBelow40V => {
                // We want to use the BMR491's VOUT thresholding as a backup
                // brownout detector.
                let current_max_duty = pmbus_read!(self.device, MAX_DUTY)
                    .map_err(|_| MitigationFailureKind::StateReadFailed)?;
                let current_vout_uv_fault_limit =
                    pmbus_read!(self.device, VOUT_UV_FAULT_LIMIT)
                        .map_err(|_| MitigationFailureKind::StateReadFailed)?;
                let current_vout_uv_fault_response =
                    pmbus_read!(self.device, VOUT_UV_FAULT_RESPONSE)
                        .map_err(|_| MitigationFailureKind::StateReadFailed)?;

                current_max_duty.0 == EXPECTED_MAX_DUTY
                    && current_vout_uv_fault_limit.0 == EXPECTED_UV_FAULT_LIMIT
                    && current_vout_uv_fault_response.0
                        == EXPECTED_UV_FAULT_RESPONSE
            }
            ExternalInputVoltageProtection::CutoffAt40V => {
                // We do not need to use the VOUT thresholding.
                true
            }
        };

        if basic_mitigation_ok && vout_detect_ok {
            // The device configuration already reflects the mitigation.
            return Ok(MitigationAction::AlreadyApplied);
        }

        // Override the VIN_OFF threshold to 0V, so that the IBC's internal
        // controller treats VIN as "always above threshold."
        pmbus_write!(self.device, VIN_OFF, VIN_OFF::CommandData(0))
            .map_err(|_| MitigationFailureKind::UpdateFailed)?;

        match external_protection {
            ExternalInputVoltageProtection::CutoffBelow40V => {
                // Override the max duty cycle, to effectively force the power
                // supply to start the output voltage drooping earlier than it
                // otherwise would. This max duty cycle was selected to be ~95%.
                pmbus_write!(
                    self.device,
                    MAX_DUTY,
                    MAX_DUTY::CommandData(EXPECTED_MAX_DUTY)
                )
                .map_err(|_| MitigationFailureKind::UpdateFailed)?;
                // Adjust the VOUT fault to detect undervoltage on the 12V rail.
                pmbus_write!(
                    self.device,
                    VOUT_UV_FAULT_LIMIT,
                    VOUT_UV_FAULT_LIMIT::CommandData(EXPECTED_UV_FAULT_LIMIT)
                )
                .map_err(|_| MitigationFailureKind::UpdateFailed)?;
                // And configure it to shut off the IBC without retry on fault.
                // (The retry options are all wrong for our use case, they can
                // cause it to power itself on and off repeatedly before
                // stopping, or just stop; we choose the latter.)
                pmbus_write!(
                    self.device,
                    VOUT_UV_FAULT_RESPONSE,
                    VOUT_UV_FAULT_RESPONSE::CommandData(
                        EXPECTED_UV_FAULT_RESPONSE
                    )
                )
                .map_err(|_| MitigationFailureKind::UpdateFailed)?;
            }
            ExternalInputVoltageProtection::CutoffAt40V => {
                // No additional configuration is required.
            }
        }

        // DO NOT PERSIST THIS CONFIGURATION. We will re-check it and re-apply
        // it as needed on any restart or update.

        Ok(MitigationAction::NewlyApplied)
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

    pub fn set_vout(&self, v: u16) -> Result<(), Error> {
        let mut vout = VOUT_COMMAND::CommandData(0);
        let value = Volts(v as f32);
        vout.set(self.read_mode()?, pmbus::units::Volts(value.0))?;
        pmbus_write!(self.device, VOUT_COMMAND, vout)
    }

    pub fn read_vout(&self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, bmr491::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
    }
}

impl Validate<Error> for Bmr491 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let expected = b"Flex";
        pmbus_validate(device, CommandCode::MFR_ID, expected)
            .map_err(Into::into)
    }
}

impl TempSensor<Error> for Bmr491 {
    fn read_temperature(&self) -> Result<Celsius, Error> {
        let temp = pmbus_read!(self.device, bmr491::READ_TEMPERATURE_1)?;
        Ok(Celsius(temp.get()?.0))
    }
}

impl CurrentSensor<Error> for Bmr491 {
    fn read_iout(&self) -> Result<Amperes, Error> {
        let iout = pmbus_read!(self.device, bmr491::READ_IOUT)?;
        Ok(Amperes(iout.get()?.0))
    }
}

impl VoltageSensor<Error> for Bmr491 {
    fn read_vout(&self) -> Result<Volts, Error> {
        let vout = pmbus_read!(self.device, bmr491::READ_VOUT)?;
        Ok(Volts(vout.get(self.read_mode()?)?.0))
    }
}
