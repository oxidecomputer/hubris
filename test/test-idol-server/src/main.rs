// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]
#![forbid(clippy::wildcard_imports)]

use idol_runtime::{NotificationHandler, RequestError};
use test_idol_api::{FancyTestType, IdolTestError, SocketName, UdpMetadata};
use userlib::RecvMessage;

struct ServerImpl;

impl idl::InOrderIdolTestImpl for ServerImpl {
    fn increment(
        &mut self,
        _: &RecvMessage,
        i: usize,
    ) -> Result<usize, RequestError<IdolTestError>> {
        Ok(i + 1)
    }
    fn maybe_increment(
        &mut self,
        _: &RecvMessage,
        i: usize,
        b: bool,
    ) -> Result<usize, RequestError<IdolTestError>> {
        Ok(if b { i + 1 } else { i })
    }
    fn return_err_if_true(
        &mut self,
        _: &RecvMessage,
        b: bool,
    ) -> Result<(), RequestError<IdolTestError>> {
        if b {
            Err(IdolTestError::YouAskedForThis.into())
        } else {
            Ok(())
        }
    }
    fn bool_not(
        &mut self,
        _: &RecvMessage,
        b: bool,
    ) -> Result<bool, RequestError<IdolTestError>> {
        Ok(!b)
    }
    fn bool_xor(
        &mut self,
        _: &RecvMessage,
        a: bool,
        b: bool,
    ) -> Result<bool, RequestError<IdolTestError>> {
        Ok(a ^ b)
    }
    fn fancy_increment(
        &mut self,
        _: &RecvMessage,
        a: FancyTestType,
    ) -> Result<FancyTestType, RequestError<IdolTestError>> {
        Ok(FancyTestType {
            u: a.u + a.b as u32,
            ..a
        })
    }
    fn extract_vid(
        &mut self,
        _: &RecvMessage,
        _a: u8,
        b: UdpMetadata,
    ) -> Result<u16, RequestError<IdolTestError>> {
        Ok(b.vid)
    }
    fn extract_vid_enum(
        &mut self,
        _: &RecvMessage,
        _a: SocketName,
        b: UdpMetadata,
    ) -> Result<u16, RequestError<IdolTestError>> {
        Ok(b.vid)
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
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
    use super::FancyTestType;
    use test_idol_api::{IdolTestError, SocketName, UdpMetadata};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
