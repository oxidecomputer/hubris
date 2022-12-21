// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    AliasCert, AliasOkm, RngSeed, SpMeasureCert, SpMeasureOkm,
    TrustQuorumDheCert, TrustQuorumDheOkm,
};
use core::ops::Range;
use dice_mfg_msgs::SizedBlob;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use stage0_handoff::DICE_RANGE as MEM_RANGE;
use stage0_handoff::{fits_in_ram, HandoffData};
use static_assertions as sa;

// This memory is the USB peripheral SRAM that's 0x4000 bytes long. Changes
// to this address must be coordinated with the [dice_*] tables in
// chips/lpc55/chip.toml
// TODO: get from app.toml -> chip.toml at build time
const CERTS_RANGE: Range<usize> = MEM_RANGE.start..(MEM_RANGE.start + 0xa00);
const ALIAS_RANGE: Range<usize> = CERTS_RANGE.end..(CERTS_RANGE.end + 0x800);
const SPMEASURE_RANGE: Range<usize> =
    ALIAS_RANGE.end..(ALIAS_RANGE.end + 0x800);
const RNG_RANGE: Range<usize> =
    SPMEASURE_RANGE.end..(SPMEASURE_RANGE.end + 0x100);

// ensure memory ranges are within MEM_RANGE and do not overlap
sa::const_assert!(MEM_RANGE.start <= CERTS_RANGE.start);
sa::const_assert!(CERTS_RANGE.end <= ALIAS_RANGE.start);
sa::const_assert!(ALIAS_RANGE.end <= SPMEASURE_RANGE.start);
sa::const_assert!(SPMEASURE_RANGE.end <= RNG_RANGE.start);
sa::const_assert!(RNG_RANGE.end <= MEM_RANGE.end);

#[derive(Deserialize, Serialize, SerializedSize)]
pub struct CertData {
    pub deviceid_cert: SizedBlob,
    pub persistid_cert: SizedBlob,
    pub intermediate_cert: SizedBlob,
}

// Handoff DICE cert chain.
//
// SAFETY: The memory range denoted by MEM_RANGE is checked to be nonoverlapping
// by static assertion above. We ensure this region is sufficiently large to
// hold CertData with another static assert.
unsafe impl HandoffData for CertData {
    const VERSION: u32 = 0;
    const MAGIC: [u8; 12] = [
        0x61, 0x3c, 0xc9, 0x2e, 0x42, 0x97, 0x96, 0xf5, 0xfa, 0xc8, 0x76, 0x69,
    ];
    const MEM_RANGE: Range<usize> = CERTS_RANGE;
}

// ensure CertData handoff memory is large enough to store data
fits_in_ram!(CertData);

impl CertData {
    // This function is unfortunately error prone: All parameters are the
    // same type and so if we get the order wrong verification of the cert
    // chain down the line will probably fail as they'll end up in the
    // wrong order. We can reduce this possibility by using the DeviceIdCert
    // type directly but the persistid and intermediate cert will remain
    // problematic.
    pub fn new(
        deviceid_cert: SizedBlob,
        persistid_cert: SizedBlob,
        intermediate_cert: SizedBlob,
    ) -> Self {
        Self {
            deviceid_cert,
            persistid_cert,
            intermediate_cert,
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
    pub alias_seed: AliasOkm,
    pub alias_cert: AliasCert,
    pub tqdhe_seed: TrustQuorumDheOkm,
    pub tqdhe_cert: TrustQuorumDheCert,
}

// Handoff DICE Alias artifacts.
//
// SAFETY: The memory range denoted by MEM_RANGE is checked to be nonoverlapping
// by static assertion above. We ensure this region is sufficiently large to
// hold AliasData with another static assert.
unsafe impl HandoffData for AliasData {
    const VERSION: u32 = 0;
    const MAGIC: [u8; 12] = [
        0x3e, 0xbc, 0x3c, 0xdc, 0x60, 0x37, 0xab, 0x86, 0xf0, 0x60, 0x20, 0x52,
    ];
    const MEM_RANGE: Range<usize> = ALIAS_RANGE;
}

// ensure AliasData handoff memory is large enough to store data
fits_in_ram!(AliasData);

impl AliasData {
    pub fn new(
        alias_seed: AliasOkm,
        alias_cert: AliasCert,
        tqdhe_seed: TrustQuorumDheOkm,
        tqdhe_cert: TrustQuorumDheCert,
    ) -> Self {
        Self {
            alias_seed,
            alias_cert,
            tqdhe_seed,
            tqdhe_cert,
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
    pub seed: SpMeasureOkm,
    pub spmeasure_cert: SpMeasureCert,
}

// Handoff DICE artifacts to task measuring the SP.
//
// SAFETY: The memory range denoted by MEM_RANGE is checked to be nonoverlapping
// by static assertion above. We ensure this region is sufficiently large to
// hold SpMeasureData with another static assert.
unsafe impl HandoffData for SpMeasureData {
    const VERSION: u32 = 0;
    const MAGIC: [u8; 12] = [
        0xec, 0x4a, 0xc2, 0x1c, 0xb5, 0xaa, 0x5b, 0x34, 0x47, 0x84, 0x96, 0x4a,
    ];
    const MEM_RANGE: Range<usize> = SPMEASURE_RANGE;
}

// ensure SpMeasureData handoff memory is large enough store data
fits_in_ram!(SpMeasureData);

impl SpMeasureData {
    pub fn new(seed: SpMeasureOkm, spmeasure_cert: SpMeasureCert) -> Self {
        Self {
            seed,
            spmeasure_cert,
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
    pub seed: RngSeed,
}

// Handoff DICE artifacts to task measuring the SP.
//
// SAFETY: The memory range denoted by MEM_RANGE is checked to be nonoverlapping
// by static assertion above. We ensure this region is sufficiently large to
// hold RngData with another static assert.
unsafe impl HandoffData for RngData {
    const VERSION: u32 = 0;
    const MAGIC: [u8; 12] = [
        0xb2, 0x48, 0x4b, 0x83, 0x3f, 0xee, 0xc0, 0xc0, 0xba, 0x0a, 0x5b, 0x6c,
    ];
    const MEM_RANGE: Range<usize> = RNG_RANGE;
}

// ensure RngData handoff memory is large enough store data
fits_in_ram!(RngData);

impl RngData {
    pub fn new(seed: RngSeed) -> Self {
        Self { seed }
    }
}
