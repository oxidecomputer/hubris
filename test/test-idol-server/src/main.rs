// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use idol_runtime::RequestError;
use test_idol_api::IdolTestError;
use userlib::*;

struct ServerImpl;

impl idl::InOrderIdolTestImpl for ServerImpl {
    fn increment(
        &mut self,
        _: &RecvMessage,
        i: usize,
    ) -> Result<usize, RequestError<IdolTestError>> {
        Ok(i + 1)
    }
}

#[export_name = "main"]
fn main() -> ! {
    // Handle messages.
    let mut incoming = [0u8; idl::INCOMING_SIZE];
    let mut serverimpl = ServerImpl;
    loop {
        idol_runtime::dispatch(&mut incoming, &mut serverimpl);
    }
}

mod idl {
    use super::IdolTestError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
