// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

pub use drv_i2c_devices::mwocp6x::{
    FirmwareImage, Mwocp67 as Mwocp6x, mwocp67::UpdateState,
};

use super::PSU_COUNT;
use super::i2c_config::devices;
use drv_i2c_api::I2cDevice;
use userlib::TaskId;

// We have the ability to update the PSU firmware but are not currently
// using it.
pub const MWOCP6X_PRIMARY_FIRMWARE: Option<FirmwareImage> = None;
pub const MWOCP6X_SECONDARY_FIRMWARE: Option<FirmwareImage> = None;

pub static DEVICES: [fn(TaskId) -> I2cDevice; PSU_COUNT] = [
    devices::mwocp67_psu0mcu,
    devices::mwocp67_psu1mcu,
    devices::mwocp67_psu2mcu,
    devices::mwocp67_psu3mcu,
    devices::mwocp67_psu4mcu,
    devices::mwocp67_psu5mcu,
];
