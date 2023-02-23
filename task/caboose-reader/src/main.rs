// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use idol_runtime::{ClientError, Leased, RequestError, W};
use task_caboose_reader_api::CabooseError;
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use userlib::*;

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    let pos = kipc::read_caboose_pos();

    let mut server = ServerImpl { pos };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Simple handle which points to the beginning of the TLV-C region of the
/// caboose and allows us to implement `TlvcRead`
#[derive(Copy, Clone)]
struct CabooseReader {
    base: u32,
    size: u32,
}

impl TlvcRead for CabooseReader {
    fn extent(&self) -> Result<u64, TlvcReadError> {
        Ok(self.size as u64)
    }
    fn read_exact(
        &self,
        offset: u64,
        dest: &mut [u8],
    ) -> Result<(), TlvcReadError> {
        let addr: u32 = self.base + u32::try_from(offset).unwrap_lite();
        for (i, out) in dest.iter_mut().enumerate() {
            *out = unsafe { *((addr as usize + i) as *const u8) };
        }
        Ok(())
    }
}

struct ServerImpl {
    pos: core::ops::Range<u32>,
}

impl idl::InOrderCabooseImpl for ServerImpl {
    fn caboose_addr(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<CabooseError>> {
        if self.pos.is_empty() {
            Err(CabooseError::MissingCaboose.into())
        } else {
            Ok(self.pos.start)
        }
    }

    fn get_key_by_tag(
        &mut self,
        _: &userlib::RecvMessage,
        name: [u8; 4],
        data: Leased<W, [u8]>,
    ) -> Result<u32, RequestError<CabooseError>> {
        if self.pos.is_empty() {
            return Err(CabooseError::MissingCaboose.into());
        }
        let reader = CabooseReader {
            base: self.pos.start,
            size: self.pos.len() as u32,
        };

        let mut reader = TlvcReader::begin(reader)
            .map_err(|_| CabooseError::TlvcReaderBeginFailed)?;
        while let Ok(Some(chunk)) = reader.next() {
            if chunk.header().tag == name {
                // TODO: verify checksum
                // TODO: make this not one byte at a time
                let mut buf = [0u8];
                for i in 0..chunk.len() {
                    chunk
                        .read_exact(i, &mut buf)
                        .map_err(|_| CabooseError::TlvcReadExactFailed)?;
                    data.write_at(i as usize, buf[0]).map_err(|_| {
                        RequestError::Fail(ClientError::BadLease)
                    })?;
                }
                return Ok(chunk.len() as u32);
            }
        }
        return Err(CabooseError::NoSuchTag.into());
    }

    fn get_key_by_u32(
        &mut self,
        msg: &userlib::RecvMessage,
        tag: u32,
        data: Leased<W, [u8]>,
    ) -> Result<u32, RequestError<CabooseError>> {
        self.get_key_by_tag(msg, tag.to_le_bytes(), data)
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::CabooseError;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
