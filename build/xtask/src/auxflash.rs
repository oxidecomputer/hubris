// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha3::{Digest, Sha3_256};
use zerocopy::AsBytes;

/// List of binary blobs to include in the auxiliary flash binary shipped with
/// this image.  The auxiliary flash is used to offload storage of large
/// configuration files (e.g. FPGA bitstreams)
#[derive(Clone, Debug, Deserialize)]
pub struct AuxFlash {
    pub blobs: Vec<AuxFlashBlob>,
}

/// A single binary blob, encoded into the auxiliary flash file.
#[derive(Clone, Debug, Deserialize)]
pub struct AuxFlashBlob {
    pub file: String,
    pub compress: bool,
    pub tag: String,
}

/// Encode the given data as a tagged TLV-C chunk
fn data_to_tlvc(tag: &str, data: &[u8]) -> Result<Vec<u8>> {
    if tag.len() != 4 {
        bail!("Tag must be a 4-byte value, not '{}'", tag);
    }
    let mut out = vec![];
    let mut header = tlvc::ChunkHeader {
        tag: tag.as_bytes().try_into().unwrap(),
        len: 0.into(),
        header_checksum: 0.into(),
    };
    out.extend(header.as_bytes());

    let c = tlvc::compute_body_crc(data);

    out.extend(data);
    let body_len = out.len() - std::mem::size_of::<tlvc::ChunkHeader>();
    let body_len = u32::try_from(body_len).unwrap();

    // TLV-C requires the body to be padded to a multiple of four!
    while out.len() & 0b11 != 0 {
        out.push(0);
    }
    out.extend(c.to_le_bytes());

    // Update the header.
    header.len.set(body_len);
    header.header_checksum.set(header.compute_checksum());

    out[..std::mem::size_of::<tlvc::ChunkHeader>()]
        .copy_from_slice(header.as_bytes());
    Ok(out)
}

/// Packs a single blob into a TLV-C structure
fn pack_blob(blob: &AuxFlashBlob) -> Result<Vec<u8>> {
    let data = std::fs::read(&blob.file)
        .with_context(|| format!("Could not read blob {}", blob.file))?;
    let data = if blob.compress {
        gnarle::compress_to_vec(&data)
    } else {
        data
    };
    data_to_tlvc(&blob.tag, &data)
}

/// Constructs an auxiliary flash image, based on RFD 311
///
/// Returns the checksum and the raw data to be saved
pub fn build_auxflash(aux: &AuxFlash) -> Result<([u8; 32], Vec<u8>)> {
    let mut auxi = vec![];
    for f in &aux.blobs {
        auxi.extend(pack_blob(f)?);
    }
    let sha = Sha3_256::digest(&auxi);

    let mut out = vec![];
    out.extend(data_to_tlvc("CHCK", &sha)?);
    out.extend(data_to_tlvc("AUXI", &auxi)?);
    Ok((sha.into(), out))
}
