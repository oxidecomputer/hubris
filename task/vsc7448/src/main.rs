// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

mod bsp;

use bsp::Bsp;
use drv_spi_api::Spi;
use ringbuf::*;
use userlib::*;
use vsc7448::{spi::Vsc7448Spi, VscError};

task_slot!(SPI, spi_driver);
const VSC7448_SPI_DEVICE: u8 = 0;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    ChipInit(u64),
    ChipInitFailed(VscError),
    BspInit(u64),
    BspInitFailed(VscError),
}
ringbuf!(Trace, 2, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let spi = Spi::from(SPI.get_task_id()).device(VSC7448_SPI_DEVICE);
    let vsc7448 = Vsc7448Spi(spi);

    let t0 = sys_get_timer().now;
    match vsc7448::init(&vsc7448) {
        Ok(()) => {
            let t1 = sys_get_timer().now;
            ringbuf_entry!(Trace::ChipInit(t1 - t0));
        }
        Err(e) => {
            ringbuf_entry!(Trace::ChipInitFailed(e));
            panic!("Could not initialize chip: {:?}", e);
        }
    }

    let t0 = sys_get_timer().now;
    match Bsp::new(&vsc7448) {
        Ok(mut bsp) => {
            let t1 = sys_get_timer().now;
            ringbuf_entry!(Trace::BspInit(t1 - t0));
            bsp.run(); // Does not terminate
        }
        Err(e) => {
            ringbuf_entry!(Trace::BspInitFailed(e));
            panic!("Could not initialize BSP: {:?}", e);
        }
    }
}
