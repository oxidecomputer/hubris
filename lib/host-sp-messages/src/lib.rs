// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Types for messages exchanged between the SP and the host CPU over the
//! control uart; see RFD 316.

#![cfg_attr(not(test), no_std)]

use hubpack::SerializedSize;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_big_array::BigArray;
use serde_repr::{Deserialize_repr, Serialize_repr};
use static_assertions::{const_assert, const_assert_eq};
use unwrap_lite::UnwrapLite;
use zerocopy::{AsBytes, FromBytes};

pub use hubpack::error::Error as HubpackError;

/// Magic value for [`Header::magic`].
pub const MAGIC: u32 = 0x01de_19cc;

/// Maximum message length.
///
/// Does not include framing overhead for packetization (e.g., cobs).
// Value from RFD316:
//   4KiB + header size + crc size + sizeof(u64)
// In RFD316, "header size" includes one byte more than our `Header` struct: the
// u8 specifying the command (for us, this byte is included in the serialized
// `SpToHost` or `HostToSp`).
pub const MAX_MESSAGE_SIZE: usize = 4123;

/// Minimum amount of space available for data trailing after an `SpToHost`
/// response.
///
/// The buffer passed to the `fill_data` callback of `serialize` is guaranteed
/// to be _at least_ this long, regardless of the particular `SpToHost` response
/// being sent. It will be longer than this for any `SpToHost` variants that
/// serialize to a sequence shorter than `SpToHost::MAX_SIZE`.
pub const MIN_SP_TO_HOST_FILL_DATA_LEN: usize =
    MAX_MESSAGE_SIZE - Header::MAX_SIZE - CHECKSUM_SIZE - SpToHost::MAX_SIZE;

const CHECKSUM_SIZE: usize = core::mem::size_of::<u16>();

pub mod version {
    pub const V1: u32 = 1;
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct Header {
    pub magic: u32,
    pub version: u32,
    pub sequence: u64,
    // Followed by either `SpToHost` or `HostToSp`, then any binary data blob
    // (depending on the variant of those enums), then a 16-bit checksum.
}

/// The order of these cases is critical! We are relying on hubpack's encoding
/// of enum variants being 0-indexed and using a single byte. The order of
/// variants in this enum produces a mapping of these that matches both RFD 316
/// and the C implementation in the host software.
///
/// Because many of these variants have associated data, we can't assign literal
/// values. Instead, we have a unit test that checks them against what we
/// expect. When updating this enum, make sure to update that test and check
/// that it contains the expected values.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum HostToSp {
    // Microoptimization: insert a dummy variant first, so we never serialize a
    // command value of `0` to make COBS's life slightly easier.
    _Unused,
    RequestReboot,
    RequestPowerOff,
    GetBootStorageUnit,
    GetIdentity,
    GetMacAddresses,
    HostBootFailure {
        reason: u8,
    },
    HostPanic {
        code: u16,
        // Followed by a binary data blob (panic message?)
    },
    GetStatus,
    // Host ack'ing SP task startup.
    AckSpStart,
    GetAlert,
    RotRequest, // Followed by a binary data blob (the request)
    RotAddHostMeasurements, // Followed by a binary data blob?
    /// Get as much phase 2 data as we can from the image identified by `hash`
    /// starting at `offset`.
    GetPhase2Data {
        hash: [u8; 32],
        offset: u64,
    },
    KeyLookup {
        // We use a raw `u8` here instead of deserializing the `Key` enum
        // (defined below) because we want to be able to distinguish
        // deserialization errors on `HostToSp` from "you sent a well-formed
        // `KeyLookup` request but with a key I don't understand". We therefore
        // deserialize this as a u8, then use `Key::from_primitive()`
        // afterwards.
        key: u8,
        max_response_len: u16,
    },
    GetInventoryData {
        index: u32,
    },
}

/// The order of these cases is critical! We are relying on hubpack's encoding
/// of enum variants being 0-indexed and using a single byte. The order of
/// variants in this enum produces a mapping of these that matches both RFD 316
/// and the C implementation in the host software.
///
/// Because many of these variants have associated data, we can't assign literal
/// values. Instead, we have a unit test that checks them against what we
/// expect. When updating this enum, make sure to update that test and check
/// that it contains the expected values.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum SpToHost {
    // Microoptimization: insert a dummy variant first, so we never serialize a
    // command value of `0` to make COBS's life slightly easier.
    _Unused,
    Ack,
    DecodeFailure(DecodeFailureReason),
    BootStorageUnit(Bsu),
    Identity(Identity),
    MacAddresses {
        base: [u8; 6],
        count: u16,
        stride: u8,
    },
    Status {
        status: Status,
        startup: HostStartupOptions,
    },
    // Followed by a binary data blob (the alert), or maybe action is another
    // hubpack-encoded enum?
    Alert {
        // details TBD
        action: u8,
    },
    // Followed by a binary data blob (the response)
    RotResponse,
    // Followed by a binary data blob (the data)
    Phase2Data,
    // If `result` is `KeyLookupResult::Ok`, this will be followed by a binary
    // blob of length at most `max_response_len` from the corresponding request.
    // For any other result, there is no subsequent binary blob.
    KeyLookupResult(KeyLookupResult),
    // If `result` is `InventoryDataResult::Ok`, this will be followed by a
    // binary blob of a hubpack-serialized `InventoryData` value.
    InventoryData {
        result: InventoryDataResult,
        name: [u8; 32],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, num_derive::FromPrimitive)]
pub enum Key {
    // Always sends back b"pong".
    Ping,
    InstallinatorImageId,
    /// Returns the max inventory size and version
    InventorySize,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum KeyLookupResult {
    Ok,
    /// We don't know the requested key.
    InvalidKey,
    /// We have no value for the requested key.
    NoValueForKey,
    /// The `max_response_len` in the request is too short for the value
    /// associated with the requested key.
    MaxResponseLenTooShort,
}

/// Results for an inventory data request
///
/// These **cannot be reordered**; the host and SP must agree on them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum InventoryDataResult {
    Ok,
    /// The given index is larger than our device count
    InvalidIndex,
    /// Communication with the device failed in a way that suggests its absence
    DeviceAbsent,
    /// Communication with the device failed in some other way
    DeviceFailed,
    /// Failed to serialize data
    SerializationError,
}

impl From<HubpackError> for InventoryDataResult {
    fn from(_: HubpackError) -> Self {
        Self::SerializationError
    }
}

impl From<drv_i2c_api::ResponseCode> for InventoryDataResult {
    fn from(e: drv_i2c_api::ResponseCode) -> Self {
        match e {
            drv_i2c_api::ResponseCode::NoDevice => {
                InventoryDataResult::DeviceAbsent
            }
            _ => InventoryDataResult::DeviceFailed,
        }
    }
}

/// Data payload for an inventory data request
///
/// These **cannot be reordered**; the host and SP must agree on them.  New
/// variants may be added to the end, and existing variants may be extended with
/// new data (at the end), but no changes should be made to existing bytes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum InventoryData {
    /// Raw DIMM data
    #[serde(with = "BigArray")]
    DimmSpd([u8; 512]),

    /// Device or board identity, typically stored in a VPD EEPROM
    VpdIdentity(Identity),

    /// 128-bit serial number baked into every AT24CSW EEPROM
    At24csw08xSerial([u8; 16]),

    /// STM32H7 UID information
    Stm32H7 {
        /// 96-bit unique identifier
        uid: [u32; 3],
        /// Revision ID (`REV_ID`) from `DBGMCU_IDC`
        dbgmcu_rev_id: u16,
        /// Device ID (`DEV_ID`) from `DBGMCU_IDC`
        dbgmcu_dev_id: u16,
    },

    /// BMR491 IBC
    Bmr491 {
        /// MFR_ID (PMBus operation 0x99)
        mfr_id: [u8; 12],
        /// MFR_MODEL (PMBus operation 0x9A)
        mfr_model: [u8; 20],
        /// MFR_REVISION (PMBus operation 0x9B)
        mfr_revision: [u8; 12],
        /// MFR_LOCATION (PMBus operation 0x9C)
        mfr_location: [u8; 12],
        /// MFR_DATE, PMBus operation 0x9D
        mfr_date: [u8; 12],
        /// MFR_SERIAL, PMBus operation 0x9E
        mfr_serial: [u8; 20],
        /// MFR_FIRMWARE_DATA, PMBus operation 0xFD
        mfr_firmware_data: [u8; 20],
    },

    /// ISL68224 power converters
    Isl68224 {
        /// MFR_ID (PMBus operation 0x99)
        mfr_id: [u8; 4],
        /// MFR_MODEL (PMBus operation 0x9A)
        mfr_model: [u8; 4],
        /// MFR_REVISION (PMBus operation 0x9B)
        mfr_revision: [u8; 4],
        /// MFR_DATE, PMBus operation 0x9D
        mfr_date: [u8; 4],
        /// IC_DEVICE_ID, PMBus operation 0xAD
        ic_device_id: [u8; 4],
        /// IC_DEVICE_REV, PMBus operation 0xAE
        ic_device_rev: [u8; 4],
    },

    /// RAA229618 power converter
    Raa229618 {
        /// MFR_ID (PMBus operation 0x99)
        mfr_id: [u8; 4],
        /// MFR_MODEL (PMBus operation 0x9A)
        mfr_model: [u8; 4],
        /// MFR_REVISION (PMBus operation 0x9B)
        mfr_revision: [u8; 4],
        /// MFR_DATE, PMBus operation 0x9D
        mfr_date: [u8; 4],
        /// IC_DEVICE_ID, PMBus operation 0xAD
        ic_device_id: [u8; 4],
        /// IC_DEVICE_REV, PMBus operation 0xAE
        ic_device_rev: [u8; 4],
    },

    Tps546b24a {
        /// MFR_ID (PMBus operation 0x99)
        mfr_id: [u8; 3],
        /// MFR_MODEL (PMBus operation 0x9A)
        mfr_model: [u8; 3],
        /// MFR_REVISION (PMBus operation 0x9B)
        mfr_revision: [u8; 3],
        /// MFR_SERIAL, PMBus operation 0x9E
        mfr_serial: [u8; 3],
        /// IC_DEVICE_ID, PMBus operation 0xAD
        ic_device_id: [u8; 6],
        /// IC_DEVICE_REV, PMBus operation 0xAE
        ic_device_rev: [u8; 2],
        /// NVM_CHECKSUM, PMBus operation 0xF0
        nvm_checksum: u16,
    },

    /// Fan subassembly identity
    FanIdentity {
        /// Identity of the fan assembly
        identity: Identity,
        /// Identity of the VPD board within the subassembly
        vpd_identity: Identity,
        /// Identity of the individual fans
        fans: [Identity; 3],
    },

    Adm1272 {
        /// MFR_ID (PMBus operation 0x99)
        mfr_id: [u8; 3],
        /// MFR_MODEL (PMBus operation 0x9A)
        mfr_model: [u8; 10],
        /// MFR_REVISION (PMBus operation 0x9B)
        mfr_revision: [u8; 2],
        /// MFR_DATE, PMBus operation 0x9D
        mfr_date: [u8; 6],
    },

    Tmp117 {
        /// Device ID (register 0x0F)
        id: u16,
        /// 48-bit NIST traceability data
        eeprom1: u16,
        eeprom2: u16,
        eeprom3: u16,
    },

    Idt8a34003 {
        hw_rev: u8,
        major_rel: u8,
        minor_rel: u8,
        hotfix_rel: u8,
        product_id: u16,
    },

    Ksz8463 {
        /// Contents of the CIDER register
        cider: u16,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct Identity {
    #[serde(with = "BigArray")]
    pub model: [u8; Identity::MODEL_LEN],
    pub revision: u32,
    #[serde(with = "BigArray")]
    pub serial: [u8; Identity::SERIAL_LEN],
}

impl From<oxide_barcode::VpdIdentity> for Identity {
    fn from(id: oxide_barcode::VpdIdentity) -> Self {
        // The Host/SP protocol has larger fields for model/serial than we
        // use currently; statically assert that we haven't outgrown them.
        const_assert!(
            oxide_barcode::VpdIdentity::PART_NUMBER_LEN <= Identity::MODEL_LEN
        );
        const_assert!(
            oxide_barcode::VpdIdentity::SERIAL_LEN <= Identity::SERIAL_LEN
        );

        let mut new_id = Self::default();
        new_id.model[..id.part_number.len()].copy_from_slice(&id.part_number);
        new_id.revision = id.revision;
        new_id.serial[..id.serial.len()].copy_from_slice(&id.serial);
        new_id
    }
}

impl Default for Identity {
    fn default() -> Self {
        Self {
            model: [0; Self::MODEL_LEN],
            revision: 0,
            serial: [0; Self::SERIAL_LEN],
        }
    }
}

impl Identity {
    pub const MODEL_LEN: usize = 51;
    pub const SERIAL_LEN: usize = 51;
}

// See RFD 316 for values.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize_repr, Serialize_repr,
)]
#[repr(u8)]
pub enum Bsu {
    A = 0x41,
    B = 0x42,
}

// We're using serde_repr for `Bsu`, so we have to supply our own
// `SerializedSize` impl (since hubpack assumes it's serializing enum variants
// itself as raw u8s).
impl hubpack::SerializedSize for Bsu {
    const MAX_SIZE: usize = core::mem::size_of::<Bsu>();
}

// See RFD 316 for values.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize_repr, Serialize_repr,
)]
#[repr(u8)]
pub enum DecodeFailureReason {
    Cobs = 0x01,
    Crc = 0x02,
    Deserialize = 0x03,
    MagicMismatch = 0x04,
    VersionMismatch = 0x05,
    SequenceInvalid = 0x06,
    DataLengthInvalid = 0x07,
}

// We're using serde_repr for `Bsu`, so we have to supply our own
// `SerializedSize` impl (since hubpack assumes it's serializing enum variants
// itself as raw u8s).
impl hubpack::SerializedSize for DecodeFailureReason {
    const MAX_SIZE: usize = core::mem::size_of::<DecodeFailureReason>();
}

impl From<HubpackError> for DecodeFailureReason {
    fn from(_: HubpackError) -> Self {
        Self::Deserialize
    }
}

bitflags::bitflags! {
    #[derive(Serialize, Deserialize, SerializedSize, FromBytes, AsBytes)]
    #[repr(transparent)]
    pub struct Status: u64 {
        const SP_TASK_RESTARTED = 1 << 0;
        const ALERTS_AVAILABLE  = 1 << 1;

        // Resync is a WIP; omit for now.
        // const READY_FOR_RESYNC  = 1 << 2;
    }

    // When adding fields to this struct, update the static assertions below to
    // ensure our conversions to/from `gateway_messages::StartupOptions` remain
    // valid!
    #[derive(Serialize, Deserialize, SerializedSize, FromBytes, AsBytes)]
    #[repr(transparent)]
    pub struct HostStartupOptions: u64 {
        const PHASE2_RECOVERY_MODE = 1 << 0;
        const STARTUP_KBM = 1 << 1;
        const STARTUP_BOOTRD = 1 << 2;
        const STARTUP_PROM = 1 << 3;
        const STARTUP_KMDB = 1 << 4;
        const STARTUP_KMDB_BOOT = 1 << 5;
        const STARTUP_BOOT_RAMDISK = 1 << 6;
        const STARTUP_BOOT_NET = 1 << 7;
        const STARTUP_VERBOSE = 1 << 8;
    }
}

// `HostStartupOptions` and `gateway_messages::StartupOptions` should be
// identical; statically assert that each field matches (i.e., each bit is in
// the same position) and that the full set of all bits match (i.e., neither
// struct has bits the other doesn't).
const_assert_eq!(
    HostStartupOptions::PHASE2_RECOVERY_MODE.bits(),
    gateway_messages::StartupOptions::PHASE2_RECOVERY_MODE.bits()
);
const_assert_eq!(
    HostStartupOptions::STARTUP_KBM.bits(),
    gateway_messages::StartupOptions::STARTUP_KBM.bits()
);
const_assert_eq!(
    HostStartupOptions::STARTUP_BOOTRD.bits(),
    gateway_messages::StartupOptions::STARTUP_BOOTRD.bits()
);
const_assert_eq!(
    HostStartupOptions::STARTUP_PROM.bits(),
    gateway_messages::StartupOptions::STARTUP_PROM.bits()
);
const_assert_eq!(
    HostStartupOptions::STARTUP_KMDB.bits(),
    gateway_messages::StartupOptions::STARTUP_KMDB.bits()
);
const_assert_eq!(
    HostStartupOptions::STARTUP_KMDB_BOOT.bits(),
    gateway_messages::StartupOptions::STARTUP_KMDB_BOOT.bits()
);
const_assert_eq!(
    HostStartupOptions::STARTUP_BOOT_RAMDISK.bits(),
    gateway_messages::StartupOptions::STARTUP_BOOT_RAMDISK.bits()
);
const_assert_eq!(
    HostStartupOptions::STARTUP_BOOT_NET.bits(),
    gateway_messages::StartupOptions::STARTUP_BOOT_NET.bits()
);
const_assert_eq!(
    HostStartupOptions::STARTUP_VERBOSE.bits(),
    gateway_messages::StartupOptions::STARTUP_VERBOSE.bits()
);
const_assert_eq!(
    HostStartupOptions::all().bits(),
    gateway_messages::StartupOptions::all().bits()
);

impl From<gateway_messages::StartupOptions> for HostStartupOptions {
    fn from(opts: gateway_messages::StartupOptions) -> Self {
        // Our static assertions above guarantee that all our bits between these
        // two types match, so we can safely convert via raw bit values.
        Self::from_bits(opts.bits()).unwrap_lite()
    }
}

impl From<HostStartupOptions> for gateway_messages::StartupOptions {
    fn from(opts: HostStartupOptions) -> Self {
        // Our static assertions above guarantee that all our bits between these
        // two types match, so we can safely convert via raw bit values.
        Self::from_bits(opts.bits()).unwrap_lite()
    }
}

/// Serializes a response packet containing
///
/// ```text
/// [header | command | data]
/// ```
///
/// where `data` is provided by the `fill_data` closure, which should populate
/// the slice it's given and return the length of data written to that slice.
///
/// If the callback fails, then serialize its result (instead of `command`)
/// immediately after `header`.  These are the same type, meaning the size
/// checks still applies.
///
/// On success, returns the length of the message serialized into `out`.
///
/// # Errors
///
/// Only fails if `command` fails to serialize into
/// `out[header_length..out.len() - 2]` (i.e., the space available between the
/// header and our trailing checksum).
///
/// # Panics
///
/// Panics if `fill_data` returns a size greater than the length of the slice it
/// was given.
pub fn try_serialize<F, S>(
    out: &mut [u8; MAX_MESSAGE_SIZE],
    header: &Header,
    command: &S,
    fill_data: F,
) -> Result<usize, HubpackError>
where
    F: FnOnce(&mut [u8]) -> Result<usize, S>,
    S: Serialize,
{
    let header_len = hubpack::serialize(out, header)?;
    let mut n = header_len;

    // We know `Header::MAX_SIZE` is much smaller than out.len(), so this
    // subtraction can't underflow. We don't know how big `command` will be, but
    // (a) `hubpack::serialize()` will fail if it's too large, and (b) if
    // serialization succeeds, this subtraction guarantees space for our
    // trailing checksum.
    let out_data_end = out.len() - CHECKSUM_SIZE;

    n += hubpack::serialize(&mut out[n..out_data_end], command)?;

    match fill_data(&mut out[n..out_data_end]) {
        Ok(data_this_message) => {
            assert!(data_this_message <= out_data_end - n);
            n += data_this_message;
        }
        Err(e) => {
            n = header_len;
            n += hubpack::serialize(&mut out[n..out_data_end], &e)?;
        }
    }

    // Compute checksum over the full message.
    let checksum = fletcher::calc_fletcher16(&out[..n]);
    out[n..][..CHECKSUM_SIZE].copy_from_slice(&checksum.to_le_bytes()[..]);
    n += CHECKSUM_SIZE;

    Ok(n)
}

/// Deserializes a response packet containing
///
/// ```text
/// [header | command | data]
/// ```
///
/// and returning those three separate parts.
///
/// # Errors
///
/// Returns [`HubpackError::Custom`] for checksum mismatches.
pub fn deserialize<T: DeserializeOwned>(
    data: &[u8],
) -> Result<(Header, T, &[u8]), DecodeFailureReason> {
    let (header, leftover) = hubpack::deserialize::<Header>(data)?;
    let (command, leftover) = hubpack::deserialize::<T>(leftover)?;

    // We expect at least 2 bytes remaining in `leftover` for the checksum; any
    // additional bytes are treated as the data blob we return.
    if leftover.len() < CHECKSUM_SIZE {
        return Err(DecodeFailureReason::DataLengthInvalid);
    }

    let (data_blob, checksum) =
        leftover.split_at(leftover.len() - CHECKSUM_SIZE);

    let checksum = u16::from_le_bytes(checksum.try_into().unwrap_lite());
    let expected_checksum =
        fletcher::calc_fletcher16(&data[..data.len() - CHECKSUM_SIZE]);

    if checksum != expected_checksum {
        return Err(DecodeFailureReason::Crc);
    }

    Ok((header, command, data_blob))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test that confirms our hubpack encoding of `HostToSp` (based on the
    // ordering of its variants) matches the expected command values described
    // in RFD 316.
    #[test]
    fn host_to_sp_command_values() {
        let mut buf = [0; HostToSp::MAX_SIZE];

        for (expected_cmd, variant) in [
            (0x01, HostToSp::RequestReboot),
            (0x02, HostToSp::RequestPowerOff),
            (0x03, HostToSp::GetBootStorageUnit),
            (0x04, HostToSp::GetIdentity),
            (0x05, HostToSp::GetMacAddresses),
            (0x06, HostToSp::HostBootFailure { reason: 0 }),
            (0x07, HostToSp::HostPanic { code: 0 }),
            (0x08, HostToSp::GetStatus),
            (0x09, HostToSp::AckSpStart),
            (0x0a, HostToSp::GetAlert),
            (0x0b, HostToSp::RotRequest),
            (0x0c, HostToSp::RotAddHostMeasurements),
            (
                0x0d,
                HostToSp::GetPhase2Data {
                    hash: [0; 32],
                    offset: 0,
                },
            ),
            (
                0x0e,
                HostToSp::KeyLookup {
                    key: 0,
                    max_response_len: 0,
                },
            ),
        ] {
            let n = hubpack::serialize(&mut buf[..], &variant).unwrap();
            assert!(n >= 1);
            assert_eq!(expected_cmd, buf[0]);
        }
    }

    // Test that confirms our hubpack encoding of `HostToSp` (based on the
    // ordering of its variants) matches the expected command values described
    // in RFD 316.
    #[test]
    fn sp_to_host_command_values() {
        let mut buf = [0; SpToHost::MAX_SIZE];

        for (expected_cmd, variant) in [
            (0x01, SpToHost::Ack),
            (0x02, SpToHost::DecodeFailure(DecodeFailureReason::Cobs)),
            (0x03, SpToHost::BootStorageUnit(Bsu::A)),
            (0x04, SpToHost::Identity(Identity::default())),
            (
                0x05,
                SpToHost::MacAddresses {
                    base: [0; 6],
                    count: 0,
                    stride: 0,
                },
            ),
            (
                0x06,
                SpToHost::Status {
                    status: Status::empty(),
                    startup: HostStartupOptions::empty(),
                },
            ),
            (0x07, SpToHost::Alert { action: 0 }),
            (0x08, SpToHost::RotResponse),
            (0x09, SpToHost::Phase2Data),
            (0x0a, SpToHost::KeyLookupResult(KeyLookupResult::Ok)),
        ] {
            let n = hubpack::serialize(&mut buf[..], &variant).unwrap();
            assert!(n >= 1);
            assert_eq!(expected_cmd, buf[0]);
        }
    }

    #[test]
    fn roundtrip() {
        let header = Header {
            magic: MAGIC,
            version: 123,
            sequence: 456,
        };
        let host_to_sp = HostToSp::HostPanic { code: 78 };
        let data_blob = &[1, 2, 3, 4, 5, 6, 7, 8, 9];

        let mut buf = [0; MAX_MESSAGE_SIZE];
        let n = serialize(&mut buf, &header, &host_to_sp, |out| {
            let n = data_blob.len();
            out[..n].copy_from_slice(data_blob);
            n
        })
        .unwrap();

        let deserialized = deserialize(&buf[..n]).unwrap();

        assert_eq!(header, deserialized.0);
        assert_eq!(host_to_sp, deserialized.1);
        assert_eq!(data_blob, deserialized.2);
    }

    #[test]
    fn roundtrip_large_data_blob() {
        let header = Header {
            magic: MAGIC,
            version: 123,
            sequence: 456,
        };
        let host_to_sp = HostToSp::HostPanic { code: 78 };
        let data_blob = (0_u32..)
            .into_iter()
            .map(|x| x as u8)
            .take(MAX_MESSAGE_SIZE)
            .collect::<Vec<_>>();

        let mut buf = [0; MAX_MESSAGE_SIZE];
        let mut leftover: &[u8] = &[];
        let n = serialize(&mut buf, &header, &host_to_sp, |out| {
            let n = usize::min(out.len(), data_blob.len());
            out[..n].copy_from_slice(&data_blob[..n]);
            leftover = &data_blob[n..];
            n
        })
        .unwrap();
        assert!(!leftover.is_empty());

        let deserialized = deserialize(&buf[..n]).unwrap();

        assert_eq!(header, deserialized.0);
        assert_eq!(host_to_sp, deserialized.1);
        assert_eq!(
            &data_blob[..MAX_MESSAGE_SIZE - leftover.len()],
            deserialized.2,
        );
        assert_eq!(&data_blob[MAX_MESSAGE_SIZE - leftover.len()..], leftover);
    }

    // Manually spot-check any potentially-troublesome hubpack encodings to
    // ensure they match what we expect.
    #[test]
    fn check_serialized_bytes() {
        let mut buf = [0; MAX_MESSAGE_SIZE];
        let header = Header {
            magic: MAGIC,
            version: 0x0123_4567,
            sequence: 0x1122_3344_5566_7788,
        };

        // Message including `Bsu`, which uses `Serialize_repr`.
        let message = SpToHost::BootStorageUnit(Bsu::A);
        let n = serialize(&mut buf, &header, &message, |_| 0).unwrap();
        #[rustfmt::skip]
        let expected_without_cksum: &[u8] = &[
            // magic
            0xcc, 0x19, 0xde, 0x01,
            // version
            0x67, 0x45, 0x23, 0x01,
            // sequence
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
            // command
            0x03,
            // payload (BSU A)
            0x41,
        ];
        assert_eq!(expected_without_cksum, &buf[..n - CHECKSUM_SIZE]);

        // Message including `DecodeFailureReason`, which uses `Serialize_repr`.
        let message =
            SpToHost::DecodeFailure(DecodeFailureReason::SequenceInvalid);
        let n = serialize(&mut buf, &header, &message, |_| 0).unwrap();
        #[rustfmt::skip]
        let expected_without_cksum: &[u8] = &[
            // magic
            0xcc, 0x19, 0xde, 0x01,
            // version
            0x67, 0x45, 0x23, 0x01,
            // sequence
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
            // command
            0x02,
            // payload (sequence invalid)
            0x06,
        ];
        assert_eq!(expected_without_cksum, &buf[..n - CHECKSUM_SIZE]);

        // Message including `Status`, which is defined by `bitflags!`.
        let message = SpToHost::Status {
            status: Status::SP_TASK_RESTARTED | Status::ALERTS_AVAILABLE,
            startup: HostStartupOptions::STARTUP_KMDB
                | HostStartupOptions::STARTUP_KMDB_BOOT,
        };
        let n = serialize(&mut buf, &header, &message, |_| 0).unwrap();
        #[rustfmt::skip]
        let expected_without_cksum: &[u8] = &[
            // magic
            0xcc, 0x19, 0xde, 0x01,
            // version
            0x67, 0x45, 0x23, 0x01,
            // sequence
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
            // command
            0x06,
            // payload
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(expected_without_cksum, &buf[..n - CHECKSUM_SIZE]);

        // Message including `Identity`, which uses serde_big_array.
        let fake_model = b"913-0000019";
        let fake_serial = b"OXE99990000";
        let mut identity = Identity::default();
        identity.model[..fake_model.len()].copy_from_slice(&fake_model[..]);
        identity.revision = 2;
        identity.serial[..fake_serial.len()].copy_from_slice(&fake_serial[..]);
        let message = SpToHost::Identity(identity);
        let n = serialize(&mut buf, &header, &message, |_| 0).unwrap();
        #[rustfmt::skip]
        let expected_without_cksum: &[u8] = &[
            // magic
            0xcc, 0x19, 0xde, 0x01,
            // version
            0x67, 0x45, 0x23, 0x01,
            // sequence
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
            // command
            0x04,
            // model (51 bytes)
            b'9', b'1', b'3', b'-', b'0', b'0', b'0', b'0', b'0', b'1', b'9',
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            // revision (4 bytes)
            0x02, 0x00, 0x00, 0x00,
            // serial (51 bytes)
            b'O', b'X', b'E', b'9', b'9', b'9', b'9', b'0', b'0', b'0', b'0',
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(expected_without_cksum, &buf[..n - CHECKSUM_SIZE]);
    }

    #[test]
    fn bad_host_sp_command() {
        #[rustfmt::skip]
        let data: &[u8] = &[
            // magic
            0xcc, 0x19, 0xde, 0x01,
            // version
            0x67, 0x45, 0x23, 0x01,
            // sequence
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
            // command that does not map to a `HostToSp` variant
            0xff,
        ];

        assert_eq!(
            deserialize::<HostToSp>(data),
            Err(DecodeFailureReason::Deserialize)
        );
    }

    #[test]
    fn bad_crc() {
        #[rustfmt::skip]
        let data: &[u8] = &[
            // magic
            0xcc, 0x19, 0xde, 0x01,
            // version
            0x67, 0x45, 0x23, 0x01,
            // sequence
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
            // HostToSp::RequestReboot
            0x01,
            // Incorrect checksum
            0xff, 0xff,
        ];

        assert_eq!(
            deserialize::<HostToSp>(data),
            Err(DecodeFailureReason::Crc)
        );
    }

    #[test]
    fn missing_crc() {
        #[rustfmt::skip]
        let data: &[u8] = &[
            // magic
            0xcc, 0x19, 0xde, 0x01,
            // version
            0x67, 0x45, 0x23, 0x01,
            // sequence
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
            // HostToSp::RequestReboot
            0x01,
            // CRC should be here: omit it
        ];

        assert_eq!(
            deserialize::<HostToSp>(data),
            Err(DecodeFailureReason::DataLengthInvalid)
        );
    }
}
