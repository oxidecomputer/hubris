// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// MB86RS64T FRAM demo task

#![no_std]
#![no_main]

use drv_spi_api::SpiServer;
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::UnwrapLite;

userlib::task_slot!(SPI, spi_driver);

const TEXT: &[u8] = b"system working?\n";

#[derive(Copy, Clone, Eq, PartialEq)]
enum Trace {
    None,
    CallingJoe,
    ByteOk(u8),
    ByteBad { byte: u8, expected: u8 },
    SystemWorking(bool),
}

ringbuf!(Trace, { TEXT.len() + 8 }, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let spi = drv_spi_api::Spi::from(SPI.get_task_id());
    let spi_device = spi.device(drv_spi_api::devices::MB86RS64T);
    let fram = drv_mb85rsxx_fram::Mb85rs64t::new(spi_device).unwrap_lite();
    fram.write_enable()
        .unwrap_lite()
        .write(0, TEXT)
        .unwrap_lite();

    loop {
        ringbuf_entry!(Trace::CallingJoe);

        let mut buf = [0; TEXT.len()];
        fram.read(0, &mut buf).unwrap_lite();
        let mut system_working = true;
        for (&byte, &expected) in buf.iter().zip(TEXT.iter()) {
            if byte == expected {
                ringbuf_entry!(Trace::ByteOk(expected));
            } else {
                ringbuf_entry!(Trace::ByteBad { byte, expected });
                system_working = false;
            };
        }

        ringbuf_entry!(Trace::SystemWorking(system_working));

        userlib::sys_recv_notification(notifications::HEY_YOU_MASK);
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
