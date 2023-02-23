// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use derive_idol_err::IdolError;
use idol_runtime::{ClientError, Leased, RequestError, R, W};
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use userlib::*;

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl;

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

////////////////////////////////////////////////////////////////////////////////

// TODO: move this to a caboose-api crate
#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum CabooseError {
    MissingCaboose = 1,
}

struct ServerImpl;

impl idl::InOrderCabooseImpl for ServerImpl {
    fn caboose_addr(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<CabooseError>> {
        let p = kipc::read_caboose_pos();
        Ok(p)
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::CabooseError;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
