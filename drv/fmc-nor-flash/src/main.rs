// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal driver for FMC-attached NOR flash

#![no_std]
#![no_main]

use core::convert::Infallible;
use userlib::*;

use idol_runtime::{NotificationHandler, RequestError};

#[export_name = "main"]
fn main() -> ! {
    // Wait for the FMC to be configured
    userlib::hl::sleep_for(1000);

    // Fire up a server.
    let mut server = ServerImpl;
    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl;

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

impl idl::InOrderFmcNorFlashImpl for ServerImpl {
    fn ready(
        &mut self,
        _mgs: &RecvMessage,
    ) -> Result<(), RequestError<Infallible>> {
        Ok(())
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
