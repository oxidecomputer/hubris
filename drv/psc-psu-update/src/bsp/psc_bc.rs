// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

pub use drv_i2c_devices::mwocp6x::{
    FirmwareImage, Mwocp68 as Mwocp6x, PsuMcu, mwocp68::UpdateState,
};

use super::PSU_COUNT;
use super::i2c_config::devices;
use drv_i2c_api::I2cDevice;
use userlib::TaskId;

// This image will be automatically installed on the primary MCU, if it's
// running a different firmware revision.
pub const MWOCP6X_PRIMARY_FIRMWARE: Option<FirmwareImage> =
    Some(FirmwareImage {
        mcu: PsuMcu::Primary,
        revision: *b"0762",
        payload: include_bytes!(
            "../../images/mwocp68/mwocp68-primary-0762.bin"
        ),
    });

// We don't currently care what firmware revision is on the secondary MCU, and
// secondary MCU updates are not yet implemented in `Mwocp68::update()`.
pub const MWOCP6X_SECONDARY_FIRMWARE: Option<FirmwareImage> = None;

pub static DEVICES: [fn(TaskId) -> I2cDevice; PSU_COUNT] = [
    devices::mwocp68_psu0mcu,
    devices::mwocp68_psu1mcu,
    devices::mwocp68_psu2mcu,
    devices::mwocp68_psu3mcu,
    devices::mwocp68_psu4mcu,
    devices::mwocp68_psu5mcu,
];
