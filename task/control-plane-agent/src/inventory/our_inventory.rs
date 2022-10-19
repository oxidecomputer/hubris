// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Submodule that defines logical components we claim to have (as opposed to
//! components entirely managed by other tasks, like I2C devices).

use gateway_messages::{
    sp_impl::DeviceDescription, DeviceCapabilities, DevicePresence, SpComponent,
};

pub(super) struct OurInventory {}

impl OurInventory {
    pub(super) fn new() -> Self {
        let () = ASSERT_EACH_DEVICE_FITS_IN_ONE_PACKET;
        Self {}
    }

    pub(super) fn num_devices(&self) -> usize {
        DEVICES.len()
    }

    pub(super) fn device_description(
        &self,
        index: usize,
    ) -> DeviceDescription<'static> {
        DEVICES[index]
    }
}

// List of logical or high-level components that this task is responsible for
// (or at least responds to in terms of MGS requests for status / update, even
// if another task is actually responsible for lower-level details).
//
// TODO: Are our device names and descriptions good enough, or are there more
//       specific names we should use? This may be answered when we expand
//       DeviceDescription with any VPD / serial numbers.
const DEVICES: &'static [DeviceDescription<'static>] = &[
    // We always include "ourself" as a component; this is the component name
    // MGS uses to send SP image updates.
    DeviceDescription {
        component: SpComponent::SP_ITSELF,
        device: SpComponent::SP_ITSELF.const_as_str(),
        description: "Service Processor",
        capabilities: DeviceCapabilities::UPDATEABLE,
        presence: DevicePresence::Present,
    },

    // If we have the auxflash feature enabled, report the auxflash as a
    // component. We do not mark it as explicitly "updateable", even though it
    // is written as a part of the SP update process. Crucially, that is a part
    // of updating the `SP_ITSELF` component; the auxflash is not independently
    // updateable.
    #[cfg(feature = "auxflash")]
    DeviceDescription {
        component: SpComponent::SP_AUX_FLASH,
        device: SpComponent::SP_AUX_FLASH.const_as_str(),
        description: "Service Processor auxiliary flash",
        capabilities: DeviceCapabilities::empty(),
        presence: DevicePresence::Present,
    },

    // If we're building for gimlet, we always claim to have a host CPU.
    //
    // This is a lie on gimletlet (where we still build with the "gimlet"
    // feature), but a useful one in general.
    #[cfg(feature = "gimlet")]
    DeviceDescription {
        component: SpComponent::SP3_HOST_CPU,
        device: SpComponent::SP3_HOST_CPU.const_as_str(),
        description: "Gimlet SP3 host cpu",
        capabilities: DeviceCapabilities::HAS_SERIAL_CONSOLE,
        presence: DevicePresence::Present, // TODO: ok to assume always present?
    },

    // If we're building for gimlet, we always claim to have host boot flash.
    //
    // This is a lie on gimletlet (where we still build with the "gimlet"
    // feature), and a less useful one than the host CPU (since trying to access
    // the "host flash" will fail unless we have an adapter providing QSPI
    // flash).
    #[cfg(feature = "gimlet")]
    DeviceDescription {
        component: SpComponent::HOST_CPU_BOOT_FLASH,
        device: SpComponent::HOST_CPU_BOOT_FLASH.const_as_str(),
        description: "Gimlet host boot flash",
        capabilities: DeviceCapabilities::UPDATEABLE,
        presence: DevicePresence::Present, // TODO: ok to assume always present?
    },
];

// We will spread the contents of `DEVICES` out over multiple packets to MGS;
// however, we do _not_ currently handle the case where a single `DEVICES` entry
// is too large to fit in a packet, even if it's the only device present in that
// packet. Therefore, we assert at compile time via all the machinery below that
// each entry of `DEVICES` is small enough that it will indeed fit in one packet
// after being packed into a TLV triple.
const ASSERT_EACH_DEVICE_FITS_IN_ONE_PACKET: () =
    assert_each_device_tlv_fits_in_one_packet();

const fn assert_each_device_tlv_fits_in_one_packet() {
    let mut i = 0;
    loop {
        if i == DEVICES.len() {
            break;
        }
        super::assert_device_tlv_fits_in_one_packet(
            DEVICES[i].device,
            DEVICES[i].description,
        );
        i += 1;
    }
}
