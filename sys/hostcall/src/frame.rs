// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! COBS framing of raw [`nprpc`] frames on a byte stream.
//!
//! Each frame is COBS-encoded and terminated by a single zero byte, which
//! never occurs inside the encoded data. A corrupted frame therefore costs at
//! most one message: the reader resynchronizes at the next zero.

use std::io::{BufRead, Write};

use crate::IoError;

/// Encodes `raw` and writes it, followed by the frame delimiter, flushing the
/// writer so the peer sees the frame immediately.
pub fn write_frame(writer: &mut impl Write, raw: &[u8]) -> Result<(), IoError> {
    let mut encoded = cobs::encode_vec(raw);
    encoded.push(0);
    writer.write_all(&encoded)?;
    writer.flush()?;
    Ok(())
}

/// Reads one frame, decoding it into `into`.
///
/// Returns `Ok(None)` on a clean end of stream before any byte of a frame was
/// read; a stream that ends mid-frame is an error.
pub fn read_frame<'a>(
    reader: &mut impl BufRead,
    into: &'a mut [u8],
) -> Result<Option<&'a [u8]>, IoError> {
    let mut encoded = Vec::new();
    let n = reader.read_until(0, &mut encoded)?;
    if n == 0 {
        return Ok(None);
    }
    let Some(0) = encoded.pop() else {
        return Err(IoError::Eof);
    };
    if cobs::max_encoding_length(into.len()) < encoded.len() {
        return Err(IoError::TooLarge);
    }
    let report = cobs::decode(&encoded, into).map_err(|_| IoError::Decode)?;
    Ok(Some(&into[..report.frame_size()]))
}
