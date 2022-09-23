// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Types for messages exchanged between the SP and the host CPU over the
//! control uart; see RFD 316.

#![cfg_attr(not(test), no_std)]

use hubpack::SerializedSize;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use unwrap_lite::UnwrapLite;

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
    /// Instruct SP to perform `status &= mask`
    ClearStatus {
        mask: u64,
    },
    GetAlert {
        mask: u64,
    },
    RotRequest, // Followed by a binary data blob (the request)
    RotAddHostMeasurements, // Followed by a binary data blob?
    GetPhase2Data {
        start: u64, // units TBD
        count: u64,
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
// TODO phase 2 image-related response(s)
pub enum SpToHost {
    // Microoptimization: insert a dummy variant first, so we never serialize a
    // command value of `0` to make COBS's life slightly easier.
    _Unused,
    Ack,
    DecodeFailure(DecodeFailureReason),
    BootStorageUnit(Bsu),
    Identity {
        model: u8,
        revision: u8,
        serial: [u8; 11], // TODO serial format?
    },
    MacAddresses {
        base: [u8; 6],
        count: u8, // TODO maybe a u16 instead?
        stride: u8,
    },
    Status(Status),
    // Followed by a binary data blob (the alert), or maybe action is another
    // hubpack-encoded enum?
    Alert {
        // details TBD
        action: u8,
    },
    // Followed by a binary data blob (the response)
    RotResponse,
    // Followed by a binary data blob (the data)
    Phase2Data {
        start: u64, // units TBD
    },
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

bitflags::bitflags! {
    #[derive(Serialize, Deserialize, SerializedSize)]
    pub struct Status: u64 {
        const SP_TASK_RESTARTED = 1 << 0;
        const ALERTS_AVAILABLE  = 1 << 1;
    }
}

/// On success, returns the length of the message serialized into `out` and any
/// leftover data from `data_blob` that did not fit into this message.
///
/// # Errors
///
/// Only fails if `command` fails to serialize into
/// `out[header_length..out.len() - 2]` (i.e., the space available between the
/// header and our trailing checksum). If the serialized command is maximal size
/// (i.e., exactly `MAX_MESSAGE_SIZE - header_length - 2`), none of `data_blob`
/// will be included in the serialized message.
pub fn serialize<'a>(
    out: &mut [u8; MAX_MESSAGE_SIZE],
    header: &Header,
    command: &impl Serialize,
    data_blob: &'a [u8],
) -> Result<(usize, &'a [u8]), HubpackError> {
    let mut n = hubpack::serialize(out, header)?;

    // We know `Header::MAX_SIZE` is much smaller than out.len(), so this
    // subtraction can't underflow. We don't know how big `command` will be, but
    // (a) `hubpack::serialize()` will fail if it's too large, and (b) if
    // serialization succeeds, this subtraction guarantees space for our
    // trailing checksum.
    let out_data_end = out.len() - CHECKSUM_SIZE;

    n += hubpack::serialize(&mut out[n..out_data_end], command)?;

    // After packing in `header` and `command`, how much more data can we fit
    // (while still leaving room for our checksum)?
    let data_this_message = usize::min(out_data_end - n, data_blob.len());

    // Pack in as much of `data_blob` as we can.
    out[n..][..data_this_message]
        .copy_from_slice(&data_blob[..data_this_message]);
    n += data_this_message;

    // Compute checksum over the full message.
    let checksum = fletcher::calc_fletcher16(&out[..n]);
    out[n..][..CHECKSUM_SIZE].copy_from_slice(&checksum.to_le_bytes()[..]);
    n += CHECKSUM_SIZE;

    Ok((n, &data_blob[data_this_message..]))
}

/// # Errors
///
/// Returns [`HubpackError::Custom`] for checksum mismatches.
pub fn deserialize<T: DeserializeOwned>(
    data: &[u8],
) -> Result<(Header, T, &[u8]), HubpackError> {
    let (header, leftover) = hubpack::deserialize::<Header>(data)?;
    let (command, leftover) = hubpack::deserialize::<T>(leftover)?;

    // We expect at least 2 bytes remaining in `leftover` for the checksum; any
    // additional bytes are treated as the data blob we return.
    if leftover.len() < CHECKSUM_SIZE {
        return Err(HubpackError::Truncated);
    }

    let (data_blob, checksum) =
        leftover.split_at(leftover.len() - CHECKSUM_SIZE);

    let checksum = u16::from_le_bytes(checksum.try_into().unwrap_lite());
    let expected_checksum =
        fletcher::calc_fletcher16(&data[..data.len() - CHECKSUM_SIZE]);
    if checksum != expected_checksum {
        return Err(HubpackError::Custom);
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
            (
                0x07,
                HostToSp::HostPanic {
                    status: 0,
                    cpu: 0,
                    thread: 0,
                },
            ),
            (0x08, HostToSp::GetStatus),
            (0x09, HostToSp::ClearStatus { mask: 0 }),
            (0x0a, HostToSp::GetAlert { mask: 0 }),
            (0x0b, HostToSp::RotRequest),
            (0x0c, HostToSp::RotAddHostMeasurements),
            (0x0d, HostToSp::GetPhase2Data { start: 0, count: 0 }),
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
            (
                0x04,
                SpToHost::Identity {
                    model: 0,
                    revision: 0,
                    serial: [0; 11],
                },
            ),
            (
                0x05,
                SpToHost::MacAddresses {
                    base: [0; 6],
                    count: 0,
                    stride: 0,
                },
            ),
            (0x06, SpToHost::Status(Status::empty())),
            (0x07, SpToHost::Alert { action: 0 }),
            (0x08, SpToHost::RotResponse),
            (0x09, SpToHost::Phase2Data { start: 0 }),
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
        let host_to_sp = HostToSp::HostPanic {
            status: 78,
            cpu: 90,
            thread: 12345,
        };
        let data_blob = &[1, 2, 3, 4, 5, 6, 7, 8, 9];

        let mut buf = [0; MAX_MESSAGE_SIZE];
        let (n, leftover) =
            serialize(&mut buf, &header, &host_to_sp, data_blob).unwrap();
        assert!(leftover.is_empty());

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
        let host_to_sp = HostToSp::HostPanic {
            status: 78,
            cpu: 90,
            thread: 12345,
        };
        let data_blob = (0_u32..)
            .into_iter()
            .map(|x| x as u8)
            .take(MAX_MESSAGE_SIZE)
            .collect::<Vec<_>>();

        let mut buf = [0; MAX_MESSAGE_SIZE];
        let (n, leftover) =
            serialize(&mut buf, &header, &host_to_sp, &data_blob).unwrap();
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
        let (n, leftover) =
            serialize(&mut buf, &header, &message, &[]).unwrap();
        assert!(leftover.is_empty());
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
        let (n, leftover) =
            serialize(&mut buf, &header, &message, &[]).unwrap();
        assert!(leftover.is_empty());
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
        let message = SpToHost::Status(
            Status::SP_TASK_RESTARTED | Status::ALERTS_AVAILABLE,
        );
        let (n, leftover) =
            serialize(&mut buf, &header, &message, &[]).unwrap();
        assert!(leftover.is_empty());
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
        ];
        assert_eq!(expected_without_cksum, &buf[..n - CHECKSUM_SIZE]);
    }
}
