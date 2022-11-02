// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Validate;
use drv_i2c_api::{I2cDevice, ResponseCode};
use userlib::units::Celsius;

pub use crate::nvme_bmc::{Error, NvmeBmc};

/// Wrapper for an NVME BMC device on the far end of an I2C mux that exhibits
/// lock-up behavior; see `hardware-gimlet#1804`.  The end result is that we can
/// only talk to the device when we know _for sure_ that it is powered; the `Hp`
/// in its name is our standard abbreviation for "hot-plug".
pub struct M2HpOnly {
    dev: NvmeBmc,
}

impl M2HpOnly {
    pub fn new(device: &I2cDevice) -> Self {
        Self {
            dev: NvmeBmc::new(device),
        }
    }
    /// This must only be called when you're sure that the device is powered!
    pub fn read_temperature(&self) -> Result<Celsius, Error> {
        self.dev.read_temperature()
    }
}

impl Validate<ResponseCode> for M2HpOnly {
    fn validate(
        _device: &drv_i2c_api::I2cDevice,
    ) -> Result<bool, ResponseCode> {
        // Due to a hardware limitation, we can only *attempt* to communicate
        // with the M.2s when they are powered; otherwise, the entire I2C bus
        // locks up, which is bad for business.
        //
        // Because we don't know anything about power state here in `validate`,
        // we'll just assume they're not powered.
        //
        // Returning `NoRegister` here results in `ValidateError::Unavailable`
        // being reported to the host, which seems reasonable.
        Err(ResponseCode::NoRegister)
    }
}
