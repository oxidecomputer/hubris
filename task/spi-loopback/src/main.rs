// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use ringbuf::*;
use userlib::*;

task_slot!(SPI, spi0_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Tx([u8; 8]),
    Rx([u8; 8]),
    Start,
    None,
}

ringbuf!(Trace, 16, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let spi = drv_spi_api::Spi::from(SPI.get_task_id());

    let mut a_bytes: [u8; 8] = [0; 8];
    let mut b_bytes: [u8; 8] = [0; 8];

    loop {
        ringbuf_entry!(Trace::Start);
        let ret = spi.exchange(0, &a_bytes, &mut b_bytes);
        if ret.is_err() {
            sys_panic(b"exchange failed!");
        }

        ringbuf_entry!(Trace::Tx(a_bytes));
        ringbuf_entry!(Trace::Rx(b_bytes));

        let ret = spi.exchange(0, &b_bytes, &mut a_bytes);
        if ret.is_err() {
            sys_panic(b"exhange failed!");
        }

        ringbuf_entry!(Trace::Tx(b_bytes));
        ringbuf_entry!(Trace::Rx(a_bytes));
    }
}
