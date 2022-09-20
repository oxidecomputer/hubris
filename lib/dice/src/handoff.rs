// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    AliasCert, AliasOkm, DeviceIdSelfCert, RngSeed, SpMeasureCert,
    SpMeasureOkm, TrustQuorumDheCert, TrustQuorumDheOkm,
};
use core::ops::Range;
use hubpack::SerializedSize;
use lpc55_pac::syscon::RegisterBlock;
use serde::{Deserialize, Serialize};
use static_assertions as sa;

// This memory is the USB peripheral SRAM that's 0x4000 bytes long. Changes
// to this address must be coordinated with the [dice_*] tables in
// chips/lpc55/chip.toml
// TODO: get from app.toml -> chip.toml at build time
const MEM_RANGE: Range<usize> = 0x4010_0000..0x4010_4000;
const ALIAS_RANGE: Range<usize> = MEM_RANGE.start..(MEM_RANGE.start + 0x800);
const SPMEASURE_RANGE: Range<usize> =
    ALIAS_RANGE.end..(ALIAS_RANGE.end + 0x800);
const RNG_RANGE: Range<usize> =
    SPMEASURE_RANGE.end..(SPMEASURE_RANGE.end + 0x100);

// ensure memory ranges are within MEM_RANGE and do not overlap
sa::const_assert!(MEM_RANGE.start <= ALIAS_RANGE.start);
sa::const_assert!(ALIAS_RANGE.end <= SPMEASURE_RANGE.start);
sa::const_assert!(SPMEASURE_RANGE.end <= RNG_RANGE.start);
sa::const_assert!(RNG_RANGE.end <= MEM_RANGE.end);

/// The Handoff type is a thin wrapper over the memory region used to transfer
/// DICE artifacts (seeds & certs) from stage0 to hubris tasks. It is intended
/// for use by stage0 to write these artifacts to memory where they will later
/// be read out by a hubris task.
pub struct Handoff<'a>(&'a RegisterBlock);

impl<'a> Handoff<'a> {
    // Handing off DICE artifacts through the USB SRAM requires we power it on.
    // We implement this as a constructor on the producer side of the handoff
    // to ensure this memory is enabled before consumers attempt access.
    // Attempts to access this memory region before powering it on will fault.
    pub fn turn_on(syscon: &'a RegisterBlock) -> Self {
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
        // Cast MEM_RANGE from HandoffData to a mutable slice.
        //
        // SAFETY: This unsafe block relies on implementers of the HandoffData
        // trait to validate the memory range denoted by Self::MEM_RANGE. Each
        // implementation in this module is checked by static assertion.
        let dst = unsafe {
            core::slice::from_raw_parts_mut(
                T::MEM_RANGE.start as *mut u8,
                <T as SerializedSize>::MAX_SIZE,
            )
        };
        // TODO: error handling
        hubpack::serialize(dst, t).expect("handoff store")
    }
}

// Types that can be transfered through the memory region used to pass DICE
// artifacts from stage0 to hubris tasks.
//
// This trait cannot check the validity of the memory range selected by
// implementers and so implementers of this trait are required to ensure that
// the range denoted by Self::MEM_RANGE is:
// - within the memory range used to hold DICE artifacts
// - large enough to contain a the largest serialized form of the implementing
// type
// - non-overlapping with the ranges of memory used by other implementers of
// this trait
pub unsafe trait HandoffData {
    const EXPECTED_MAGIC: [u8; 16];
    const MEM_RANGE: Range<usize>;

    fn get_magic(&self) -> [u8; 16];

    fn from_mem() -> Option<Self>
    where
        Self: SerializedSize + Sized,
        for<'d> Self: Deserialize<'d>,
    {
        // Cast the MEM_START address to a slice of bytes of MAX_SIZE length.
        //
        // SAFETY: This unsafe block relies on implementers of the trait to
        // validate the memory range denoted by Self::MEM_RANGE. Each
        // implementation in this module is checked by static assertion.
        let src = unsafe {
            core::slice::from_raw_parts_mut(
                Self::MEM_RANGE.start as *mut u8,
                <Self as SerializedSize>::MAX_SIZE,
            )
        };

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
    pub alias_seed: AliasOkm,
    pub alias_cert: AliasCert,
    pub tqdhe_seed: TrustQuorumDheOkm,
    pub tqdhe_cert: TrustQuorumDheCert,
    pub deviceid_cert: DeviceIdSelfCert,
}

// Handoff DICE Alias artifacts.
//
// SAFETY: The memory range denoted by MEM_RANGE is checked to be nonoverlapping
// by static assertion above. We ensure this region is sufficiently large to
// hold AliasData with another static assert.
unsafe impl HandoffData for AliasData {
    const EXPECTED_MAGIC: [u8; 16] = [
        0x3e, 0xbc, 0x3c, 0xdc, 0x60, 0x37, 0xab, 0x86, 0xf0, 0x60, 0x20, 0x52,
        0xc4, 0xfd, 0xd5, 0x58,
    ];
    const MEM_RANGE: Range<usize> = ALIAS_RANGE;

    fn get_magic(&self) -> [u8; 16] {
        self.magic
    }
}

// ensure AliasData handoff memory is large enough to store data
sa::const_assert!(
    AliasData::MEM_RANGE.end - AliasData::MEM_RANGE.start
        >= <AliasData as SerializedSize>::MAX_SIZE
);

impl AliasData {
    pub fn new(
        alias_seed: AliasOkm,
        alias_cert: AliasCert,
        tqdhe_seed: TrustQuorumDheOkm,
        tqdhe_cert: TrustQuorumDheCert,
        deviceid_cert: DeviceIdSelfCert,
    ) -> Self {
        Self {
            magic: Self::EXPECTED_MAGIC,
            alias_seed,
            alias_cert,
            tqdhe_seed,
            tqdhe_cert,
            deviceid_cert,
        }
    }
}

/// Type to represent DICE derived artifacts used by the task that measures the
/// SP image. This task may use these artifacts to continue the DICE certificate
/// higherarchy to the SP. Stage0 will construct an instance of this type and
/// write it to memory using the Handoff type above. The receiving hubris task
/// will then construct an instance from the serialized value using the 'from_mem'
/// constructor.
#[derive(Deserialize, Serialize, SerializedSize)]
pub struct SpMeasureData {
    pub magic: [u8; 16],
    pub seed: SpMeasureOkm,
    pub spmeasure_cert: SpMeasureCert,
    pub deviceid_cert: DeviceIdSelfCert,
}

// Handoff DICE artifacts to task measuring the SP.
//
// SAFETY: The memory range denoted by MEM_RANGE is checked to be nonoverlapping
// by static assertion above. We ensure this region is sufficiently large to
// hold SpMeasureData with another static assert.
unsafe impl HandoffData for SpMeasureData {
    const EXPECTED_MAGIC: [u8; 16] = [
        0xec, 0x4a, 0xc2, 0x1c, 0xb5, 0xaa, 0x5b, 0x34, 0x47, 0x84, 0x96, 0x4a,
        0x0a, 0x55, 0x54, 0x37,
    ];
    const MEM_RANGE: Range<usize> = SPMEASURE_RANGE;

    fn get_magic(&self) -> [u8; 16] {
        self.magic
    }
}

// ensure SpMeasureData handoff memory is large enough store data
sa::const_assert!(
    SpMeasureData::MEM_RANGE.end - SpMeasureData::MEM_RANGE.start
        >= <SpMeasureData as SerializedSize>::MAX_SIZE
);

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

/// Type to represent DICE derived artifacts used by the RNG task. This set of
/// artifacts is limited to a single, high entropy seed that the RNG task may
/// use to seed the RNG. Stage0 will construct an instance of this type and
/// write it to memory using the Handoff type above. The receiving hubris task
/// will then construct an instance from the serialized value using the 'from_mem'
/// constructor.
#[derive(Deserialize, Serialize, SerializedSize)]
pub struct RngData {
    pub magic: [u8; 16],
    pub seed: RngSeed,
}

// Handoff DICE artifacts to task measuring the SP.
//
// SAFETY: The memory range denoted by MEM_RANGE is checked to be nonoverlapping
// by static assertion above. We ensure this region is sufficiently large to
// hold RngData with another static assert.
unsafe impl HandoffData for RngData {
    const EXPECTED_MAGIC: [u8; 16] = [
        0xb2, 0x48, 0x4b, 0x83, 0x3f, 0xee, 0xc0, 0xc0, 0xba, 0x0a, 0x5b, 0x6c,
        0x34, 0x98, 0x45, 0x6c,
    ];
    const MEM_RANGE: Range<usize> = RNG_RANGE;

    fn get_magic(&self) -> [u8; 16] {
        self.magic
    }
}

// ensure RngData handoff memory is large enough store data
sa::const_assert!(
    RngData::MEM_RANGE.end - RngData::MEM_RANGE.start
        >= <RngData as SerializedSize>::MAX_SIZE
);

impl RngData {
    pub fn new(seed: RngSeed) -> Self {
        Self {
            magic: Self::EXPECTED_MAGIC,
            seed,
        }
    }
}
