// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Types for messages exchanged between the SP and the host CPU over the
//! control uart; see RFD 316.

#![cfg_attr(not(test), no_std)]

use hubpack::SerializedSize;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use unwrap_lite::UnwrapLite;

pub use hubpack::error::Error as HubpackError;

/// Maximum message length.
///
/// Does not include framing overhead for packetization (e.g., cobs).
pub const MAX_MESSAGE_SIZE: usize = 1024;

const CHECKSUM_SIZE: usize = core::mem::size_of::<u16>();

pub mod version {
    pub const V1: u32 = 1;
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub struct Header {
    pub version: u32,
    pub sequence: u64,
    // Followed by either `SpToHost` or `HostToSp`, then any binary data blob
    // (depending on the variant of those enums), then a 16-bit checksum.
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum HostToSp {
    // Microoptimization: insert a dummy variant first, so we never serialize a
    // command value of `0` to make COBS's life slightly easier.
    _Unused,
    GetBootStorageUnit,
    HostBootFailure {
        reason: u8,
    },
    HostPanic {
        status: u16,
        cpu: u16,
        thread: u64,
        // Followed by a binary data blob (panic message?)
    },
    GetIdentity,
    GetStatus,
    /// Instruct SP to perform `status &= mask`
    ClearStatus {
        mask: u64,
    },
    GetMacAddresses,
    RebootHost,
    PowerOffHost,
    RotRequest, // Followed by a binary data blob (the request)
}

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
    BootStorageUnit(u8),
    Identity {
        model: u8,
        revision: u8,
        serial: [u8; 11], // TODO serial format?
    },
    Status(u64), // TODO replace u64 with bitflags type?
    MacAddresses([[u8; 6]; 16]), // TODO send as a data blob? how fixed is 16?
    RotResponse, // Followed by a binary data blob (the response)
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, SerializedSize,
)]
pub enum DecodeFailureReason {
    // Microoptimization: insert a dummy variant first, so we never serialize a
    // command value of `0` to make COBS's life slightly easier.
    _Unused,
    CobsError,
    HubpackError,
    CrcFailure,
    VersionMismatch,
    BadRequestType,
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

    #[test]
    fn roundtrip() {
        let header = Header {
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
}
