// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the auxiliary flash IC

#![no_std]

use derive_idol_err::IdolError;
use sha3::{Digest, Sha3_256};
use tlvc::{TlvcRead, TlvcReader};
use userlib::{sys_send, FromPrimitive};
use zerocopy::{AsBytes, FromBytes};

pub use drv_qspi_api::{PAGE_SIZE_BYTES, SECTOR_SIZE_BYTES};

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum AuxFlashError {
    WriteEnableFailed = 1,
    TlvcReaderBeginFailed,

    /// The requested slot exceeds the slot count
    InvalidSlot,
    /// The `CHCK` block does not have 32 bytes of data
    BadChckSize,
    /// There is no `CHCK` block in this slot
    MissingChck,
    /// There is no `AUXI` block in this slot
    MissingAuxi,
    /// There is more than one `CHCK` block in this slot
    MultipleChck,
    /// There is more than one `AUXI` block in this slot
    MultipleAuxi,
    /// The `CHCK` checksum disagrees with the actual slot data (`AUXI`)
    ChckMismatch,
    /// Failed during a call to `ChunkHandle::read_exact`
    ChunkReadFail,
    /// The end address of the read or write exceeds the slot boundaries
    AddressOverflow,
    /// The start address of a write command is not aligned to a page boundary
    UnalignedAddress,
    /// There is no active slot
    NoActiveSlot,
    /// There is no blob with this name
    NoSuchBlob,
    /// Writes to the currently-active slot are not allowed
    SlotActive,

    #[idol(server_death)]
    ServerRestarted,
}

#[derive(Copy, Clone, FromBytes, AsBytes)]
#[repr(transparent)]
pub struct AuxFlashId(pub [u8; 20]);

#[derive(Copy, Clone, PartialEq, Eq, FromBytes, AsBytes)]
#[repr(transparent)]
pub struct AuxFlashChecksum(pub [u8; 32]);

#[derive(Copy, Clone, FromBytes, AsBytes)]
#[repr(transparent)]
pub struct AuxFlashTag(pub [u8; 4]);

#[derive(Copy, Clone, FromBytes, AsBytes)]
#[repr(C)]
pub struct AuxFlashBlob {
    pub slot: u32,
    pub start: u32,
    pub end: u32,
}

////////////////////////////////////////////////////////////////////////////////

/// Extension trait to do auxflash operations on anything that
/// implements `TlvcRead`.
pub trait TlvcReadAuxFlash {
    fn calculate_checksum(self) -> Result<AuxFlashChecksum, AuxFlashError>;
    fn read_stored_checksum(self) -> Result<AuxFlashChecksum, AuxFlashError>;
    fn get_blob_by_tag(
        self,
        slot: u32,
        tag: [u8; 4],
    ) -> Result<AuxFlashBlob, AuxFlashError>;
}

impl<R> TlvcReadAuxFlash for R
where
    R: TlvcRead,
{
    fn read_stored_checksum(self) -> Result<AuxFlashChecksum, AuxFlashError> {
        let mut reader = TlvcReader::begin(self)
            .map_err(|_| AuxFlashError::TlvcReaderBeginFailed)?;

        while let Ok(Some(chunk)) = reader.next() {
            if &chunk.header().tag == b"CHCK" {
                if chunk.len() != 32 {
                    return Err(AuxFlashError::BadChckSize);
                }
                let mut out = [0; 32];
                chunk
                    .read_exact(0, &mut out)
                    .map_err(|_| AuxFlashError::ChunkReadFail)?;
                return Ok(AuxFlashChecksum(out));
            }
        }
        Err(AuxFlashError::MissingChck)
    }

    fn calculate_checksum(self) -> Result<AuxFlashChecksum, AuxFlashError> {
        let mut reader = TlvcReader::begin(self)
            .map_err(|_| AuxFlashError::TlvcReaderBeginFailed)?;

        while let Ok(Some(chunk)) = reader.next() {
            if &chunk.header().tag == b"AUXI" {
                // Read data and calculate the checksum using a scratch buffer
                let mut sha = Sha3_256::new();
                let mut scratch = [0u8; 256];
                let mut i: u64 = 0;
                while i < chunk.len() {
                    let amount = (chunk.len() - i).min(scratch.len() as u64);
                    chunk
                        .read_exact(i, &mut scratch[0..(amount as usize)])
                        .map_err(|_| AuxFlashError::ChunkReadFail)?;
                    i += amount;
                    sha.update(&scratch[0..(amount as usize)]);
                }
                let sha_out = sha.finalize();

                let mut out = [0; 32];
                out.copy_from_slice(sha_out.as_slice());
                return Ok(AuxFlashChecksum(out));
            }
        }
        Err(AuxFlashError::MissingAuxi)
    }

    fn get_blob_by_tag(
        self,
        slot: u32,
        tag: [u8; 4],
    ) -> Result<AuxFlashBlob, AuxFlashError> {
        let mut outer_reader = TlvcReader::begin(self)
            .map_err(|_| AuxFlashError::TlvcReaderBeginFailed)?;
        while let Ok(Some(outer_chunk)) = outer_reader.next() {
            if &outer_chunk.header().tag == b"AUXI" {
                let mut inner_reader = outer_chunk.read_as_chunks();
                while let Ok(Some(inner_chunk)) = inner_reader.next() {
                    if inner_chunk.header().tag == tag {
                        // At this point, the inner reader is positioned *after*
                        // our target chunk.  We back off by the full length of
                        // the chunk (including the header), then offset by the
                        // header size to get to the beginning of the blob data.
                        let (_, inner_offset, _) = inner_reader.into_inner();
                        let pos = inner_offset
                            - inner_chunk.header().total_len_in_bytes() as u64
                            + core::mem::size_of::<tlvc::ChunkHeader>() as u64;
                        return Ok(AuxFlashBlob {
                            slot,
                            start: pos as u32,
                            end: (pos + inner_chunk.len()) as u32,
                        });
                    }
                }
                return Err(AuxFlashError::NoSuchBlob);
            }
        }
        Err(AuxFlashError::MissingAuxi)
    }
}

/// Extension functions on the autogenerated `AuxFlash` type
impl AuxFlash {
    /// Reads a compressed blob, streaming it to a callback function
    pub fn get_compressed_blob_streaming<F, E>(
        &self,
        tag: [u8; 4],
        f: F,
    ) -> Result<[u8; 32], E>
    where
        F: Fn(&[u8]) -> Result<(), E>,
        E: From<AuxFlashError>,
    {
        let blob = self.get_blob_by_tag(tag)?;
        let mut scratch_buf = [0u8; 128];
        let mut pos = blob.start;
        let mut sha = Sha3_256::new();
        let mut decompressor = gnarle::Decompressor::default();

        while pos < blob.end {
            let amount = (blob.end - pos).min(scratch_buf.len() as u32);
            let chunk = &mut scratch_buf[0..(amount as usize)];
            self.read_slot_with_offset(blob.slot, pos, chunk)?;
            sha.update(&chunk);
            pos += amount;

            // Reborrow as an immutable chunk, then decompress
            let mut chunk = &scratch_buf[0..(amount as usize)];
            let mut decompress_buffer = [0; 512];

            while !chunk.is_empty() {
                let decompressed_chunk = gnarle::decompress(
                    &mut decompressor,
                    &mut chunk,
                    &mut decompress_buffer,
                );

                // The decompressor may have encountered a partial run at the
                // end of the `chunk`, in which case `decompressed_chunk`
                // will be empty since more data is needed before output is
                // generated.
                if !decompressed_chunk.is_empty() {
                    // Perform the callback
                    f(decompressed_chunk)?;
                }
            }
        }
        Ok(sha.finalize().into())
    }
}

////////////////////////////////////////////////////////////////////////////////

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));

mod config {
    include!(concat!(env!("OUT_DIR"), "/auxflash_config.rs"));
}

pub use self::config::SLOT_COUNT;
pub const SLOT_SIZE: usize = (self::config::MEMORY_SIZE / SLOT_COUNT) as usize;
