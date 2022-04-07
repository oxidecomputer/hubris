// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the AT24CSW080 I2C EEPROM

use drv_i2c_api::*;

pub struct At24csw080 {
    device: I2cDevice,
}

impl core::fmt::Display for At24csw080 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "at24csw080: {}", &self.device)
    }
}
