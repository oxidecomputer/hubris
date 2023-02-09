// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use idol_runtime::RequestError;
use userlib::*;

struct ServerImpl;

impl idl::InOrderOverflowImpl for ServerImpl {
    fn overflow(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<core::convert::Infallible>> {
        let mut buf = [0u8; 2024];
        sys_recv_open(&mut buf[0..], 5);
        Ok(0)
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl;
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
