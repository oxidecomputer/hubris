// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_caboose::{CabooseError, CabooseReader};
use idol_runtime::{ClientError, Leased, RequestError, W};
use userlib::*;

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    let mut server = ServerImpl {
        caboose: kipc::get_caboose(),
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl {
    caboose: Option<&'static [u8]>,
}

impl idl::InOrderCabooseImpl for ServerImpl {
    fn caboose_addr(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<CabooseError>> {
        let addr = self
            .caboose
            .map(|c| c.as_ptr() as u32)
            .ok_or(CabooseError::MissingCaboose)?;
        Ok(addr)
    }

    fn get_key_by_tag(
        &mut self,
        _: &userlib::RecvMessage,
        name: [u8; 4],
        data: Leased<W, [u8]>,
    ) -> Result<u32, RequestError<CabooseError>> {
        let reader = self
            .caboose
            .map(CabooseReader::new)
            .ok_or(CabooseError::MissingCaboose)?;

        let chunk = reader.get(name)?;
        if chunk.len() > data.len() {
            return Err(RequestError::Fail(ClientError::BadLease))?;
        }

        data.write_range(0..chunk.len(), chunk)
            .map_err(|_| RequestError::Fail(ClientError::BadLease))?;
        Ok(chunk.len() as u32)
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
