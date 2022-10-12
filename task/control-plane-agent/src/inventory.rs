// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use gateway_messages::{sp_impl::DeviceDescription, DevicePresence};
use task_validate_api::{Validate, ValidateError, ValidateOk, DEVICES};

userlib::task_slot!(VALIDATE, validate);

pub(crate) struct Inventory {
    validate_task: Validate,
}

impl Inventory {
    pub(crate) fn new() -> Self {
        Self {
            validate_task: Validate::from(VALIDATE.get_task_id()),
        }
    }

    pub(crate) fn num_devices(&self) -> usize {
        DEVICES.len()
    }

    pub(crate) fn device_description(
        &self,
        index: usize,
    ) -> DeviceDescription<'static> {
        let presence = match self.validate_task.validate_i2c(index) {
            Ok(ValidateOk::Present | ValidateOk::Validated) => {
                DevicePresence::Present
            }
            Ok(ValidateOk::Removed) | Err(ValidateError::NotPresent) => {
                DevicePresence::NotPresent
            }
            Err(ValidateError::BadValidation) => DevicePresence::Failed,
            Err(ValidateError::Unavailable | ValidateError::DeviceOff) => {
                DevicePresence::Unavailable
            }
            Err(ValidateError::DeviceTimeout) => DevicePresence::Timeout,
            Err(ValidateError::InvalidDevice | ValidateError::DeviceError) => {
                DevicePresence::Error
            }
        };
        let device = &DEVICES[index];
        DeviceDescription {
            device: device.device,
            description: device.description,
            num_measurement_channels: device.num_measurement_channels,
            presence,
        }
    }
}
