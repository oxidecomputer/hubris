// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

mod bsp;
mod server;

use crate::{bsp::Bsp, server::ServerImpl};
use drv_spi_api::Spi;
use ringbuf::*;
use userlib::*;
use vsc7448::{spi::Vsc7448Spi, Vsc7448, VscError};

task_slot!(SPI, spi_driver);
const VSC7448_SPI_DEVICE: u8 = 0;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    ChipInit(u64),
    ChipInitFailed(VscError),
    BspInit(u64),
    BspInitFailed(VscError),
    WakeErr(VscError),
}
ringbuf!(Trace, 2, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let spi = Spi::from(SPI.get_task_id()).device(VSC7448_SPI_DEVICE);
    let mut vsc7448_spi = Vsc7448Spi::new(spi);
    let vsc7448 = Vsc7448::new(&mut vsc7448_spi);

    // Used to turn on LEDs before anything else happens
    bsp::preinit();

    let t0 = sys_get_timer().now;
    match vsc7448.init(bsp::REFCLK_SEL, bsp::REFCLK2_SEL) {
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
    let bsp = match Bsp::new(&vsc7448) {
        Ok(bsp) => {
            let t1 = sys_get_timer().now;
            ringbuf_entry!(Trace::BspInit(t1 - t0));
            bsp
        }
        Err(e) => {
            ringbuf_entry!(Trace::BspInitFailed(e));
            panic!("Could not initialize BSP: {:?}", e);
        }
    };

    let mut server = ServerImpl::new(bsp, &vsc7448, &bsp::PORT_MAP);
    loop {
        if let Err(e) = server.wake() {
            ringbuf_entry!(Trace::WakeErr(e));
        }
        let mut msgbuf = [0u8; server::INCOMING_SIZE];
        idol_runtime::dispatch_n(&mut msgbuf, &mut server);
    }
}
