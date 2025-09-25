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
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

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

pub type SensorIndex = u32;

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
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
)]
pub enum HostToSp {
    // Microoptimization: insert a dummy variant first, so we never serialize a
    // command value of `0` to make COBS's life slightly easier.
    #[count(skip)]
    _Unused,
    RequestReboot,
    RequestPowerOff,
    GetBootStorageUnit,
    GetIdentity,
    GetMacAddresses,
    HostBootFailure {
        reason: u8,
    },
    HostPanic, // Followed by a binary data blob (panic data)
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
    // KeySet is followed by a binary data blob (the value to set the key to)
    KeySet {
        // We use a raw `u8` here for the same reason as in `KeyLookup` above.
        key: u8,
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
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
)]
pub enum SpToHost {
    // Microoptimization: insert a dummy variant first, so we never serialize a
    // command value of `0` to make COBS's life slightly easier.
    #[count(skip)]
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
    KeyLookupResult(#[count(children)] KeyLookupResult),
    // If `result` is `InventoryDataResult::Ok`, this will be followed by a
    // binary blob of a hubpack-serialized `InventoryData` value.
    InventoryData {
        #[count(children)]
        result: InventoryDataResult,
        name: [u8; 32],
    },
    KeySetResult(#[count(children)] KeySetResult),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, num_derive::FromPrimitive)]
pub enum Key {
    // Always sends back b"pong".
    Ping,
    InstallinatorImageId,
    /// Returns the max inventory size and version
    InventorySize,
    /// `/etc/system` file content
    EtcSystem,
    /// `/kernel/drv/dtrace.conf` file content
    DtraceConf,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
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

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
)]
pub enum KeySetResult {
    Ok,
    /// We don't know the requested key.
    InvalidKey,
    /// Key is read-only.
    ReadOnlyKey,
    /// The data in the request is too long for the value associated with the
    /// requested key.
    DataTooLong,
}

/// Results for an inventory data request
///
/// These **cannot be reordered**; the host and SP must agree on them.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    SerializedSize,
    counters::Count,
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

impl From<drv_i2c_types::ResponseCode> for InventoryDataResult {
    fn from(e: drv_i2c_types::ResponseCode) -> Self {
        match e {
            drv_i2c_types::ResponseCode::NoDevice => {
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
// Note: this type may ask you to let it derive `Copy`. DO NOT ALLOW THIS! An
// `InventoryData` is *really big* --- over 512 bytes --- and deriving `Copy`
// would make it easy to accidentally pass one by value on the stack, increasing
// stack usage when it isn't necessary to do so. We would like to only allow
// this type to be bytewise-copied explicitly by calling `.clone()`, instead.
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
#[allow(clippy::large_enum_variant)]
pub enum InventoryData {
    /// Raw DIMM data
    DimmSpd {
        #[serde(with = "BigArray")]
        id: [u8; 512],
        temp_sensor: SensorIndex,
    },

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

        temp_sensor: SensorIndex,
        power_sensor: SensorIndex,
        voltage_sensor: SensorIndex,
        current_sensor: SensorIndex,
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

        voltage_sensors: [SensorIndex; 3],
        current_sensors: [SensorIndex; 3],
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

        temp_sensors: [SensorIndex; 2],
        power_sensors: [SensorIndex; 2],
        voltage_sensors: [SensorIndex; 2],
        current_sensors: [SensorIndex; 2],
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

        temp_sensor: SensorIndex,
        voltage_sensor: SensorIndex,
        current_sensor: SensorIndex,
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

    Adm127x {
        /// MFR_ID (PMBus operation 0x99)
        mfr_id: [u8; 3],
        /// MFR_MODEL (PMBus operation 0x9A)
        mfr_model: [u8; 10],
        /// MFR_REVISION (PMBus operation 0x9B)
        mfr_revision: [u8; 2],
        /// MFR_DATE, PMBus operation 0x9D
        mfr_date: [u8; 6],

        temp_sensor: SensorIndex,
        voltage_sensor: SensorIndex,
        current_sensor: SensorIndex,
    },

    Tmp117 {
        /// Device ID (register 0x0F)
        id: u16,
        /// 48-bit NIST traceability data
        eeprom1: u16,
        eeprom2: u16,
        eeprom3: u16,

        temp_sensor: SensorIndex,
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

    Max5970 {
        voltage_sensors: [SensorIndex; 2],
        current_sensors: [SensorIndex; 2],
    },

    /// MAX31790 fan controller
    Max31790 { speed_sensors: [SensorIndex; 6] },

    Raa229620a {
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

        temp_sensors: [SensorIndex; 2],
        power_sensors: [SensorIndex; 2],
        voltage_sensors: [SensorIndex; 2],
        current_sensors: [SensorIndex; 2],
    },

    /// LTC4282 hot-swap controller
    Ltc4282 {
        voltage_sensor: SensorIndex,
        current_sensor: SensorIndex,
    },

    /// LM5066I hot-swap controller
    Lm5066I {
        /// MFR_ID (PMBus operation 0x99)
        mfr_id: [u8; 3],
        /// MFR_MODEL (PMBus operation 0x9A)
        mfr_model: [u8; 8],
        /// MFR_REVISION (PMBus operation 0x9B)
        mfr_revision: [u8; 2],

        temp_sensor: SensorIndex,
        power_sensor: SensorIndex,
        voltage_sensor: SensorIndex,
        current_sensor: SensorIndex,
    },
    /// Raw DIMM data for a DDR5 part
    DimmDdr5Spd {
        #[serde(with = "BigArray")]
        id: [u8; 1024],
        temp_sensors: [SensorIndex; 2],
    },

    /// W25Q256JVEIQ flash chip (auxiliary flash on Cosmo, Grapefruit, Sidecar)
    W25q256jveqi { unique_id: [u8; 8] },

    /// Cosmo host flash
    W25q01jvzeiq {
        /// 64-bit unique ID for die 0
        die0_unique_id: [u8; 8],

        /// 64-bit unique ID for die 1
        die1_unique_id: [u8; 8],
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
        // The incoming part number and serial are already nul-padded if they're
        // shorter than the allocated space in VpdIdentity, so we can merely
        // copy them into the start of our fields and the result is still
        // nul-padded.
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
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize_repr,
    Serialize_repr,
    counters::Count,
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

#[derive(
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    SerializedSize,
    FromBytes,
    Immutable,
    KnownLayout,
    IntoBytes,
)]
#[repr(transparent)]
pub struct Status(u64);

#[derive(
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    SerializedSize,
    FromBytes,
    Immutable,
    KnownLayout,
    IntoBytes,
)]
#[repr(transparent)]
pub struct HostStartupOptions(u64);

bitflags::bitflags! {
    impl Status: u64 {
        const SP_TASK_RESTARTED = 1 << 0;
        const ALERTS_AVAILABLE  = 1 << 1;

        // Resync is a WIP; omit for now.
        // const READY_FOR_RESYNC  = 1 << 2;
    }

    // When adding fields to this struct, update the static assertions below to
    // ensure our conversions to/from `gateway_messages::StartupOptions` remain
    // valid!
    impl HostStartupOptions: u64 {
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

pub fn serialize<F>(
    out: &mut [u8; MAX_MESSAGE_SIZE],
    header: &Header,
    command: &impl Serialize,
    fill_data: F,
) -> Result<usize, HubpackError>
where
    F: FnOnce(&mut [u8]) -> usize,
{
    try_serialize(out, header, command, |buf| Ok(fill_data(buf)))
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
            (0x07, HostToSp::HostPanic),
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
            (0x0f, HostToSp::GetInventoryData { index: 0 }),
            (0x10, HostToSp::KeySet { key: 0 }),
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
            (
                0x0b,
                SpToHost::InventoryData {
                    result: InventoryDataResult::Ok,
                    name: [0u8; 32],
                },
            ),
            (0x0c, SpToHost::KeySetResult(KeySetResult::Ok)),
        ] {
            let n = hubpack::serialize(&mut buf[..], &variant).unwrap();
            assert!(n >= 1);
            assert_eq!(expected_cmd, buf[0]);
        }
    }

    #[test]
    fn key_lookup_result_values() {
        let mut buf = [0; KeyLookupResult::MAX_SIZE];

        for (expected_cmd, variant) in [
            (0x0, KeyLookupResult::Ok),
            (0x1, KeyLookupResult::InvalidKey),
            (0x2, KeyLookupResult::NoValueForKey),
            (0x3, KeyLookupResult::MaxResponseLenTooShort),
        ] {
            let n = hubpack::serialize(&mut buf[..], &variant).unwrap();
            assert!(n <= 1);
            assert_eq!(expected_cmd, buf[0]);
        }
    }

    #[test]
    fn key_set_result_values() {
        let mut buf = [0; KeySetResult::MAX_SIZE];

        for (expected_cmd, variant) in [
            (0x0, KeySetResult::Ok),
            (0x1, KeySetResult::InvalidKey),
            (0x2, KeySetResult::ReadOnlyKey),
            (0x3, KeySetResult::DataTooLong),
        ] {
            let n = hubpack::serialize(&mut buf[..], &variant).unwrap();
            assert!(n <= 1);
            assert_eq!(expected_cmd, buf[0]);
        }
    }

    #[test]
    fn inventory_data_result_values() {
        let mut buf = [0; InventoryDataResult::MAX_SIZE];

        for (expected_cmd, variant) in [
            (0x0, InventoryDataResult::Ok),
            (0x1, InventoryDataResult::InvalidIndex),
            (0x2, InventoryDataResult::DeviceAbsent),
            (0x3, InventoryDataResult::DeviceFailed),
            (0x4, InventoryDataResult::SerializationError),
        ] {
            let n = hubpack::serialize(&mut buf[..], &variant).unwrap();
            assert!(n <= 1);
            assert_eq!(expected_cmd, buf[0]);
        }
    }

    #[test]
    fn inventory_data() {
        let mut v = [0; 512];
        v[0..5].copy_from_slice(&[1, 2, 3, 4, 123]);
        let d = InventoryData::DimmSpd {
            id: v,
            temp_sensor: 0x1234,
        };

        let mut buf = [0; InventoryData::MAX_SIZE];
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 517);
        assert_eq!(buf[0], 0); // discriminant
        assert_eq!(buf[1..513], v);
        assert_eq!(&buf[513..n], &[0x34, 0x12, 0, 0]);

        let i = Identity {
            model: [
                1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            revision: 123,
            serial: [
                5, 6, 7, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        };
        let d = InventoryData::VpdIdentity(i);
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 107);
        assert_eq!(buf[0], 1); // discriminant
        assert_eq!(&buf[1..52], &i.model);
        assert_eq!(&buf[52..56], [123, 0, 0, 0]);
        assert_eq!(&buf[56..107], &i.serial);

        let i = [1, 2, 3, 4, 5, 6, 7, 8, 0, 10, 32, 12, 43, 55, 128, 255];
        let d = InventoryData::At24csw08xSerial(i);
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 17);
        assert_eq!(buf[0], 2); // discriminant
        assert_eq!(&buf[1..n], &i);

        let d = InventoryData::Stm32H7 {
            uid: [0xabcdef, 1, 0x10000000],
            dbgmcu_rev_id: 0xaabb,
            dbgmcu_dev_id: 0xccdd,
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 17);
        assert_eq!(
            buf[..n],
            [
                3, 0xef, 0xcd, 0xab, 0, 1, 0, 0, 0, 0, 0, 0, 0x10, 0xbb, 0xaa,
                0xdd, 0xcc
            ]
        );

        let d = InventoryData::Bmr491 {
            mfr_id: [1, 2, 3, 4, 5, 4, 3, 2, 1, 9, 9, 9],
            mfr_model: [
                37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37,
                37, 37, 37, 37,
            ],
            mfr_revision: [89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89],
            mfr_location: [1, 2, 3, 4, 5, 6, 7, 8, 9, 8, 7, 6],
            mfr_date: [3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1],
            mfr_serial: *b"there are sooo many ",
            mfr_firmware_data: *b"fields in this thing",

            temp_sensor: 0x1234,
            power_sensor: 0x5678,
            voltage_sensor: 0x9abc,
            current_sensor: 0xdef0,
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 125);
        assert_eq!(
            buf[..n],
            [
                4, 1, 2, 3, 4, 5, 4, 3, 2, 1, 9, 9, 9, 37, 37, 37, 37, 37, 37,
                37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 37, 89, 89,
                89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 1, 2, 3, 4, 5, 6, 7, 8,
                9, 8, 7, 6, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 116, 104, 101,
                114, 101, 32, 97, 114, 101, 32, 115, 111, 111, 111, 32, 109,
                97, 110, 121, 32, 102, 105, 101, 108, 100, 115, 32, 105, 110,
                32, 116, 104, 105, 115, 32, 116, 104, 105, 110, 103, 0x34,
                0x12, 0, 0, 0x78, 0x56, 0, 0, 0xbc, 0x9a, 0, 0, 0xf0, 0xde, 0,
                0,
            ]
        );

        let d = InventoryData::Isl68224 {
            mfr_id: [1, 2, 3, 4],
            mfr_model: [9, 8, 7, 6],
            mfr_revision: [0, 10, 0, 15],
            mfr_date: [24, 25, 26, 27],
            ic_device_id: [50, 51, 52, 53],
            ic_device_rev: [60, 61, 62, 63],

            voltage_sensors: [0x1234, 0x5678, 0x9abc],
            current_sensors: [0x1122, 0x3344, 0x5566],
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 49);
        assert_eq!(
            buf[..n],
            [
                5, 1, 2, 3, 4, 9, 8, 7, 6, 0, 10, 0, 15, 24, 25, 26, 27, 50,
                51, 52, 53, 60, 61, 62, 63, 0x34, 0x12, 0, 0, 0x78, 0x56, 0, 0,
                0xbc, 0x9a, 0, 0, 0x22, 0x11, 0, 0, 0x44, 0x33, 0, 0, 0x66,
                0x55, 0, 0
            ]
        );

        let d = InventoryData::Raa229618 {
            mfr_id: [1, 2, 3, 4],
            mfr_model: [9, 8, 7, 6],
            mfr_revision: [0, 10, 0, 15],
            mfr_date: [24, 25, 26, 27],
            ic_device_id: [50, 51, 52, 53],
            ic_device_rev: [60, 61, 62, 63],

            temp_sensors: [0x1234, 0x5678],
            power_sensors: [0x9abc, 0xdef0],
            voltage_sensors: [0x1122, 0x3344],
            current_sensors: [0x5566, 0x6677],
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 57);
        assert_eq!(
            buf[..n],
            [
                6, 1, 2, 3, 4, 9, 8, 7, 6, 0, 10, 0, 15, 24, 25, 26, 27, 50,
                51, 52, 53, 60, 61, 62, 63, 0x34, 0x12, 0, 0, 0x78, 0x56, 0, 0,
                0xbc, 0x9a, 0, 0, 0xf0, 0xde, 0, 0, 0x22, 0x11, 0, 0, 0x44,
                0x33, 0, 0, 0x66, 0x55, 0, 0, 0x77, 0x66, 0, 0
            ]
        );

        let d = InventoryData::Tps546b24a {
            mfr_id: [1, 2, 3],
            mfr_model: [9, 8, 7],
            mfr_revision: [0, 10, 0],
            mfr_serial: [24, 25, 26],
            ic_device_id: [50, 51, 52, 53, 54, 55],
            ic_device_rev: [60, 61],
            nvm_checksum: 0xaabb,
            temp_sensor: 0x1234,
            voltage_sensor: 0x5678,
            current_sensor: 0x9abc,
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 35);
        assert_eq!(
            buf[..n],
            [
                7, 1, 2, 3, 9, 8, 7, 0, 10, 0, 24, 25, 26, 50, 51, 52, 53, 54,
                55, 60, 61, 0xbb, 0xaa, 0x34, 0x12, 0, 0, 0x78, 0x56, 0, 0,
                0xbc, 0x9a, 0, 0,
            ]
        );

        let i = Identity {
            model: [
                1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            revision: 123,
            serial: [
                5, 6, 7, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        };
        let d = InventoryData::FanIdentity {
            identity: i,
            vpd_identity: i,
            fans: [i; 3],
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 531);
        assert_eq!(buf[0], 8);
        let mut b = &buf[1..];
        while b.is_empty() {
            assert_eq!(&b[0..51], i.model);
            assert_eq!(&b[51..55], [123, 0, 0, 0]);
            assert_eq!(&b[55..106], i.serial);
            b = &b[106..];
        }

        let d = InventoryData::Adm127x {
            mfr_id: [1, 2, 3],
            mfr_model: [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
            mfr_revision: [0, 10],
            mfr_date: [10, 20, 30, 40, 50, 60],
            temp_sensor: 0x1234,
            voltage_sensor: 0x5678,
            current_sensor: 0x9abc,
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 34);
        assert_eq!(
            &buf[..n],
            [
                9, 1, 2, 3, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 10, 10, 20, 30,
                40, 50, 60, 0x34, 0x12, 0, 0, 0x78, 0x56, 0, 0, 0xbc, 0x9a, 0,
                0,
            ]
        );

        let d = InventoryData::Tmp117 {
            id: 0xaabb,
            eeprom1: 0x1234,
            eeprom2: 0x5678,
            eeprom3: 0xabcd,
            temp_sensor: 0x5599,
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 13);
        assert_eq!(
            &buf[..n],
            [
                10, 0xbb, 0xaa, 0x34, 0x12, 0x78, 0x56, 0xcd, 0xab, 0x99, 0x55,
                0x00, 0x00
            ]
        );

        let d = InventoryData::Idt8a34003 {
            hw_rev: 1,
            major_rel: 100,
            minor_rel: 200,
            hotfix_rel: 255,
            product_id: 0xaabc,
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 7);
        assert_eq!(&buf[..n], [11, 1, 100, 200, 255, 0xbc, 0xaa]);

        let d = InventoryData::Ksz8463 { cider: 0x1234 };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf[..n], [12, 0x34, 0x12]);

        let d = InventoryData::Max5970 {
            voltage_sensors: [1, 2],
            current_sensors: [3, 4],
        };
        let n = hubpack::serialize(&mut buf, &d).unwrap();
        assert_eq!(n, 17);
        assert_eq!(
            &buf[..n],
            [13, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0]
        );
    }

    #[test]
    fn roundtrip() {
        let header = Header {
            magic: MAGIC,
            version: 123,
            sequence: 456,
        };
        let host_to_sp = HostToSp::HostPanic;
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
        let host_to_sp = HostToSp::HostPanic;
        let data_blob = (0..)
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
