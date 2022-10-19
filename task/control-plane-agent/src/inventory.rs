// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::fmt::{self, Write};
use gateway_messages::{
    sp_impl::DeviceDescription, DeviceCapabilities, DevicePresence, SpComponent,
};
use task_validate_api::DEVICES as VALIDATE_DEVICES;
use task_validate_api::{Validate, ValidateError, ValidateOk};
use userlib::UnwrapLite;

// Most of the devices we report to MGS come from asking the `validate` task,
// but we have a handful that we describe ourself; they're listed by
// `OurInventory`, and we (in `Inventory` below) logically glue together
// `OurInventory`'s devices with `VALIDATE_DEVICES`.
mod our_inventory;

use our_inventory::OurInventory;

userlib::task_slot!(VALIDATE, validate);

pub(crate) struct Inventory {
    validate_task: Validate,
    ours: OurInventory,
}

impl Inventory {
    pub(crate) fn new() -> Self {
        let () = ASSERT_EACH_DEVICE_FITS_IN_ONE_PACKET;

        Self {
            validate_task: Validate::from(VALIDATE.get_task_id()),
            ours: OurInventory::new(),
        }
    }

    pub(crate) fn num_devices(&self) -> usize {
        self.ours.num_devices() + VALIDATE_DEVICES.len()
    }

    pub(crate) fn device_description(
        &self,
        index: usize,
    ) -> DeviceDescription<'static> {
        // If `index` is in `0..self.ours.num_devices()`, defer to it;
        // otherwise, subtract `self.ours.num_devices()` to shift it into the
        // range `0..VALIDATE_DEVICES.len()` and ask `validate`.
        let index = match index.checked_sub(self.ours.num_devices()) {
            // Subtraction failure; `index` is in `0..self.ours.num_devices()`.
            None => return self.ours.device_description(index),
            // Subtraction success; `index` has been shifted down to be in the
            // range `0..VALIDATE_DEVICES.len()`.
            Some(index) => index,
        };

        let device = &VALIDATE_DEVICES[index];

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

        let mut component = FmtComponentId::default();
        component
            .write_fmt(format_args!(
                "{}{}",
                SpComponent::GENERIC_DEVICE_PREFIX,
                index
            ))
            .unwrap_lite();

        let mut capabilities = DeviceCapabilities::empty();
        if device.num_measurement_channels > 0 {
            capabilities |= DeviceCapabilities::HAS_MEASUREMENT_CHANNELS;
        }
        DeviceDescription {
            component: SpComponent { id: component.id },
            device: device.device,
            description: device.description,
            capabilities,
            presence,
        }
    }
}

#[derive(Default)]
struct FmtComponentId {
    pos: usize,
    id: [u8; SpComponent::MAX_ID_LENGTH],
}

impl fmt::Write for FmtComponentId {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let remaining = &mut self.id[self.pos..];
        if s.len() <= remaining.len() {
            remaining[..s.len()].copy_from_slice(s.as_bytes());
            self.pos += s.len();
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

// We use a generic component ID of `{prefix}{index}` for all of
// `VALIDATE_DEVICES`; here we statically assert the maximum number of devices
// we can use with this scheme. At the time of writing this comment, our ID
// width is 16 bytes and the prefix is 4 bytes, allowing up to 999_999_999_999
// devices to be listed.
//
// We tag this with `#[allow(dead_code)]` to prevent warnings about the contents
// of this module not being used; the static assertion _is_ still checked.
#[allow(dead_code)]
mod max_num_devices {
    use super::{SpComponent, VALIDATE_DEVICES};

    // How many bytes are available for digits of a device index in base 10?
    const DIGITS_AVAILABLE: usize =
        SpComponent::MAX_ID_LENGTH - SpComponent::GENERIC_DEVICE_PREFIX.len();

    // How many devices can we list given `DIGITS_AVAILABLE`?
    const MAX_NUM_DEVICES: u64 = const_exp10(DIGITS_AVAILABLE);

    // Statically assert that we have at most that many devices.
    static_assertions::const_assert!(
        VALIDATE_DEVICES.len() as u64 <= MAX_NUM_DEVICES
    );

    // Helper function: computes 10^n at compile time.
    const fn const_exp10(mut n: usize) -> u64 {
        let mut x = 1;
        while n > 0 {
            x *= 10;
            n -= 1;
        }
        x
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
        gateway_messages::DeviceDescriptionHeader::MAX_SIZE
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
        if i == VALIDATE_DEVICES.len() {
            break;
        }
        assert_device_tlv_fits_in_one_packet(
            VALIDATE_DEVICES[i].device,
            VALIDATE_DEVICES[i].description,
        );
        i += 1;
    }
}
