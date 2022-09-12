// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    AliasCert, AliasOkm, DeviceIdSelfCert, SpMeasureCert, SpMeasureOkm,
};
use hubpack::SerializedSize;
use lpc55_pac::syscon::RegisterBlock;
use serde::{Deserialize, Serialize};

// This memory is the USB peripheral SRAM that's 0x4000 bytes long. Changes
// to this address must be coordinated with the [dice_*] tables in
// chips/lpc55/chip.toml
// TODO: get from app.toml -> chip.toml at build time
const MEM_START: usize = 0x4010_0000;
const ALIAS_START: usize = MEM_START;
const ALIAS_SIZE: usize = 0x800;
const SPMEASURE_START: usize = ALIAS_START + ALIAS_SIZE;
const SPMEASURE_SIZE: usize = 0x800;
const RNG_START: usize = SPMEASURE_START + SPMEASURE_SIZE;
const RNG_SIZE: usize = 0x100;

pub fn slice_from_parts(start: usize, size: usize) -> &'static mut [u8] {
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
    unsafe { core::slice::from_raw_parts_mut(start as *mut u8, size) }
}

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

    pub fn store<T>(&self, t: &T) -> usize
    where
        T: HandoffData + SerializedSize + Serialize,
    {
        let dst =
            slice_from_parts(T::MEM_START, <T as SerializedSize>::MAX_SIZE);
        // TODO: error handling
        hubpack::serialize(dst, t).expect("handoff store")
    }
}

pub trait HandoffData {
    const EXPECTED_MAGIC: [u8; 16];
    const MEM_START: usize;
    const MEM_SIZE: usize;
    const MAX_SIZE: usize;

    fn get_magic(&self) -> [u8; 16];

    fn from_mem() -> Option<Self>
    where
        Self: Sized,
        for<'d> Self: Deserialize<'d>,
    {
        let src = slice_from_parts(Self::MEM_START, Self::MAX_SIZE);

        match hubpack::deserialize::<Self>(src).ok() {
            Some((data, _)) => {
                if data.get_magic() == Self::EXPECTED_MAGIC {
                    Some(data)
                } else {
                    None
                }
            }
            None => None,
        }
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

impl HandoffData for AliasData {
    const EXPECTED_MAGIC: [u8; 16] = [
        0x3e, 0xbc, 0x3c, 0xdc, 0x60, 0x37, 0xab, 0x86, 0xf0, 0x60, 0x20, 0x52,
        0xc4, 0xfd, 0xd5, 0x58,
    ];
    const MEM_START: usize = ALIAS_START;
    const MEM_SIZE: usize = ALIAS_SIZE;
    const MAX_SIZE: usize = <AliasData as SerializedSize>::MAX_SIZE;

    fn get_magic(&self) -> [u8; 16] {
        self.magic
    }
}

impl AliasData {
    pub fn new(
        seed: AliasOkm,
        alias_cert: AliasCert,
        deviceid_cert: DeviceIdSelfCert,
    ) -> Self {
        Self {
            magic: Self::EXPECTED_MAGIC,
            seed,
            alias_cert,
            deviceid_cert,
        }
    }
}

/// Type to hold the DICE artifacts used by the task that controls the
/// interface to the service processor.
#[derive(Deserialize, Serialize, SerializedSize)]
pub struct SpMeasureData {
    pub magic: [u8; 16],
    pub seed: SpMeasureOkm,
    pub spmeasure_cert: SpMeasureCert,
    pub deviceid_cert: DeviceIdSelfCert,
}

impl HandoffData for SpMeasureData {
    const EXPECTED_MAGIC: [u8; 16] = [
        0xec, 0x4a, 0xc2, 0x1c, 0xb5, 0xaa, 0x5b, 0x34, 0x47, 0x84, 0x96, 0x4a,
        0x0a, 0x55, 0x54, 0x37,
    ];
    const MEM_START: usize = SPMEASURE_START;
    const MEM_SIZE: usize = SPMEASURE_SIZE;
    const MAX_SIZE: usize = <SpMeasureData as SerializedSize>::MAX_SIZE;

    fn get_magic(&self) -> [u8; 16] {
        self.magic
    }
}

impl SpMeasureData {
    pub fn new(
        seed: SpMeasureOkm,
        spmeasure_cert: SpMeasureCert,
        deviceid_cert: DeviceIdSelfCert,
    ) -> Self {
        Self {
            magic: Self::EXPECTED_MAGIC,
            seed,
            spmeasure_cert,
            deviceid_cert,
        }
    }
}
