//! A driver for the rp2040 GPIO

#![no_std]
#![no_main]

use idol_runtime::{NotificationHandler, RequestError};

struct ServerImpl {}

impl idl::InOrderPinsImpl for ServerImpl {
    fn enable_out(
        &mut self,
        msg: &userlib::RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        todo!()
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    let mut incoming = [0; idl::INCOMING_SIZE];

    let mut server = ServerImpl {};

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
