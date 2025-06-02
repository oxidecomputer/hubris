// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Gimlet Host Flash server.

#![no_std]

use derive_idol_err::IdolError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::{sys_send, FromPrimitive};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub use drv_qspi_api::{PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES};

/// Errors that can be produced from the host flash server API.
///
/// This enumeration doesn't include errors that result from configuration
/// issues, like sending host flash messages to some other task.
#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum HfError {
    WriteEnableFailed = 1,
    HashBadRange,
    HashError,
    HashNotConfigured,
    NotMuxedToSP,
    Sector0IsReserved,
    NoPersistentData,
    MonotonicCounterOverflow,
    BadChipId,
    BadAddress,
    QspiTimeout,
    QspiTransferError,

    #[idol(server_death)]
    ServerRestarted,
}

/// Controls whether the SP or host CPU has access to flash
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u8)]
pub enum HfMuxState {
    SP = 1,
    HostCPU = 2,
}

/// Selects between multiple flash chips. This is not used on all hardware
/// revisions; it was added in Gimlet rev B.
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    IntoBytes,
    Immutable,
    KnownLayout,
    Serialize,
    Deserialize,
    SerializedSize,
)]
#[repr(u8)]
pub enum HfDevSelect {
    Flash0 = 0,
    Flash1 = 1,
}

impl core::ops::Not for HfDevSelect {
    type Output = Self;
    fn not(self) -> Self::Output {
        match self {
            Self::Flash0 => Self::Flash1,
            Self::Flash1 => Self::Flash0,
        }
    }
}

/// Flag which allows sector 0 to be modified
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    IntoBytes,
    Immutable,
    KnownLayout,
    Serialize,
    Deserialize,
    SerializedSize,
)]
#[repr(u8)]
pub enum HfProtectMode {
    ProtectSector0,
    AllowModificationsToSector0,
}

/// Persistent data associated with host flash
#[derive(Copy, Clone, Debug, Deserialize, Serialize, SerializedSize)]
pub struct HfPersistentData {
    pub dev_select: HfDevSelect,
}

/// Represents persistent data that is both stored on the host flash and used to
/// configure host boot.
///
/// We reserve sector 0 (i.e. the lowest 64 KiB) of both host flash ICs for
/// Hubris data.  On both host flashes, we tile sector 0 with this 40-byte
/// struct, placed at 128-byte intervals (in case it needs to grow in the
/// future).
///
/// The current value of persistent data is the instance of `RawPersistentData`
/// with a valid checksum and the highest `monotonic_counter` across both flash
/// ICs.
///
/// When writing new data, we increment the monotonic counter and write to both
/// ICs, one by one.  This ensures robustness in case of power loss.
#[derive(
    Copy, Clone, Eq, PartialEq, IntoBytes, FromBytes, Immutable, KnownLayout,
)]
#[repr(C)]
pub struct HfRawPersistentData {
    /// Reserved field, because this is placed at address 0, which PSP firmware
    /// may look at under certain circumstances.
    amd_reserved_must_be_all_ones: u64,

    /// Must always be `HF_PERSISTENT_DATA_MAGIC`.
    oxide_magic: u32,

    /// Must always be `HF_PERSISTENT_DATA_HEADER_VERSION` (for now)
    header_version: u32,

    /// Monotonically increasing counter
    pub monotonic_counter: u64,

    /// Either 0 or 1; directly translatable to [`HfDevSelect`]
    pub dev_select: u32,

    /// CRC-32 over the rest of the data using the iSCSI polynomial
    checksum: u32,
}

impl core::cmp::PartialOrd for HfRawPersistentData {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl core::cmp::Ord for HfRawPersistentData {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.monotonic_counter.cmp(&other.monotonic_counter)
    }
}

impl HfRawPersistentData {
    pub fn new(data: HfPersistentData, monotonic_counter: u64) -> Self {
        let mut out = Self {
            amd_reserved_must_be_all_ones: u64::MAX,
            oxide_magic: HF_PERSISTENT_DATA_MAGIC,
            header_version: HF_PERSISTENT_DATA_HEADER_VERSION,
            monotonic_counter,
            dev_select: data.dev_select as u32,
            checksum: 0,
        };
        out.checksum = out.expected_checksum();
        assert!(out.is_valid());
        out
    }

    fn expected_checksum(&self) -> u32 {
        static CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISCSI);
        let mut c = CRC.digest();
        // We do a CRC32 of everything except the checksum, which is positioned
        // at the end of the struct and is a `u32`
        let size = core::mem::size_of::<HfRawPersistentData>()
            - core::mem::size_of::<u32>();
        c.update(&self.as_bytes()[..size]);
        c.finalize()
    }

    pub fn is_valid(&self) -> bool {
        self.amd_reserved_must_be_all_ones == u64::MAX
            && self.oxide_magic == HF_PERSISTENT_DATA_MAGIC
            && self.header_version == HF_PERSISTENT_DATA_HEADER_VERSION
            && self.dev_select <= 1
            && self.checksum == self.expected_checksum()
    }
}

pub const HF_PERSISTENT_DATA_MAGIC: u32 = 0x1dea_bcde;
pub const HF_PERSISTENT_DATA_STRIDE: usize = 128;
pub const HF_PERSISTENT_DATA_HEADER_VERSION: u32 = 1;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
