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
        let () = ASSERT_EACH_DEVICE_FITS_IN_ONE_PACKET;

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

// We will spread the contents of `DEVICES` out over multiple packets to MGS;
// however, we do _not_ currently handle the case where a single `DEVICES` entry
// is too large to fit in a packet, even if it's the only device present in that
// packet. Therefore, we assert at compile time via all the machinery below that
// each entry of `DEVICES` is small enough that it will indeed fit in one packet
// after being packed into a TLV triple.
const ASSERT_EACH_DEVICE_FITS_IN_ONE_PACKET: () =
    assert_each_device_tlv_fits_in_one_packet();

const fn assert_device_tlv_fits_in_one_packet(
    device: &'static str,
    description: &'static str,
) {
    use gateway_messages::{
        tlv, SerializedSize, MIN_SP_MESSAGE_TRAILING_TRAILING_DATA_LEN,
    };

    let encoded_len = tlv::tlv_len(
        gateway_messages::DeviceDescription::MAX_SIZE
            + device.len()
            + description.len(),
    );

    if encoded_len > MIN_SP_MESSAGE_TRAILING_TRAILING_DATA_LEN {
        panic!(concat!(
            "The device details (device and description) of at least one ",
            "device in the current app.toml are too long to fit in a single ",
            "TLV triple to send to MGS. Current Rust restrictions prevent us ",
            "from being able to specific the specific device in this error ",
            "message. Change this panic to `panic!(\"{{}}\", description)` ",
            "and rebuild to see the description of the too-long device ",
            "instead."
        ))
    }
}

const fn assert_each_device_tlv_fits_in_one_packet() {
    let mut i = 0;
    loop {
        if i == DEVICES.len() {
            break;
        }
        assert_device_tlv_fits_in_one_packet(
            DEVICES[i].device,
            DEVICES[i].description,
        );
        i += 1;
    }
}
