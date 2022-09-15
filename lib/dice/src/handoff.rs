// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{AliasCert, AliasOkm, DeviceIdSelfCert};
use hubpack::SerializedSize;
use lpc55_pac::syscon::RegisterBlock;
use serde::{Deserialize, Serialize};

// This memory is the USB peripheral SRAM that's 0x4000 bytes long. Changes
// to this address must be coordinated with the [dice_*] tables in
// chips/lpc55/chip.toml
// TODO: get from app.toml -> chip.toml at build time
const MEM_START: usize = 0x4010_0000;
const ALIAS_START: usize = MEM_START;

/// The Handoff type is a thin wrapper over the memory region used to transfer
/// DICE artifacts (seeds & certs) from stage0 to hubris tasks. It is intended
/// for use by stage0 to write these artifacts to memory where they will later
/// be read out by a hubris task.
pub struct Handoff<'a>(&'a RegisterBlock);

impl<'a> Handoff<'a> {
    pub fn turn_on(syscon: &'a RegisterBlock) -> Self {
        // handoff through USB SRAM requires we power it on
        syscon.ahbclkctrl2.modify(|_, w| w.usb1_ram().enable());
        syscon
            .presetctrl2
            .modify(|_, w| w.usb1_ram_rst().released());

        Self(syscon)
    }

    pub fn turn_off(self) {
        self.0
            .presetctrl2
            .modify(|_, w| w.usb1_ram_rst().asserted());
        self.0.ahbclkctrl2.modify(|_, w| w.usb1_ram().disable());
    }

    pub fn store(&self, alias_data: &AliasData) -> usize {
        // SAFETY: Dereferencing this raw pointer is necessary to write to the
        // memory region used to handoff DICE artifacts to Hubris tasks. This
        // pointer will references a valid memory region provided two
        // conditions are met:
        // 1) The associated memory region has been enabled / turned on if
        // necessary. This happens in the constructor / 'turn_on' function.
        // 2) The function call is made by code sufficintly privileged to
        // access the memory region (e.g. stage0).
        // If these conditions aren't met this access is still safe but a fault
        // will occur.
        let dst: &mut [u8] = unsafe {
            core::slice::from_raw_parts_mut(
                ALIAS_START as *mut u8,
                AliasData::MAX_SIZE,
            )
        };

        // TODO: error handling
        hubpack::serialize(dst, alias_data).expect("serialize alias-handoff")
    }
}

/// Type to represent DICE derived artifacts used by the root of trust for
/// reporting in the attestation process. Stage0 will construct an instance of
/// this type and write it to memory using the Handoff type above. The receiving
/// hubris task will then read an AliasHandoff out of memory using the
/// 'from_mem' constructor in the impl block.
// TODO: This needs to be made generic to handle an arbitrary cert chain
// instead of individual certs.
#[derive(Deserialize, Serialize, SerializedSize)]
pub struct AliasData {
    pub magic: [u8; 16],
    pub seed: AliasOkm,
    pub alias_cert: AliasCert,
    pub deviceid_cert: DeviceIdSelfCert,
}

impl AliasData {
    const MAGIC: [u8; 16] = [
        0x3e, 0xbc, 0x3c, 0xdc, 0x60, 0x37, 0xab, 0x86, 0xf0, 0x60, 0x20, 0x52,
        0xc4, 0xfd, 0xd5, 0x58,
    ];

    pub fn new(
        seed: AliasOkm,
        alias_cert: AliasCert,
        deviceid_cert: DeviceIdSelfCert,
    ) -> Self {
        Self {
            magic: Self::MAGIC,
            seed,
            alias_cert,
            deviceid_cert,
        }
    }
    pub fn from_mem() -> Option<Self> {
        // SAFETY: Dereferencing this raw pointer is necessary to read from the
        // memory region used to transfer the Alias DICE artifacts from stage0
        // to a Hubris task. This pointer will reference a valid memory region
        // provided two conditions are met:
        // 1) The associated memory region has been enabled / turned on if
        // necessary. This should be done by code in stage0.
        // 2) The task making the call has been granted access to the memory
        // region by the kernel.
        // If these conditions aren't met this access is still safe but a fault
        // will occur.
        let src: &[u8] = unsafe {
            core::slice::from_raw_parts(
                ALIAS_START as *const u8,
                AliasData::MAX_SIZE,
            )
        };

        // pull AliasData from memory, deserialization will succeed even if
        // memory is all 0's
        match hubpack::deserialize::<Self>(src).ok() {
            Some((data, _)) => {
                if data.magic == Self::MAGIC {
                    Some(data)
                } else {
                    None
                }
            }
            None => None,
        }
    }
}
