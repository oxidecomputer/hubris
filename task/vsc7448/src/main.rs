// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_spi_api::Spi;
use userlib::*;
use vsc7448::{bsp::Bsp, spi::Vsc7448Spi};

task_slot!(SPI, spi_driver);
const VSC7448_SPI_DEVICE: u8 = 0;

#[export_name = "main"]
fn main() -> ! {
    let spi = Spi::from(SPI.get_task_id()).device(VSC7448_SPI_DEVICE);
    let vsc7448 = Vsc7448Spi(spi);

    // `init` does a full chip reset, so we can run it multiple times
    // (although if it fails once, it's likely to fail repeatedly)
    match vsc7448::init(&vsc7448).and_then(|_| Bsp::new(&vsc7448)) {
        Ok(bsp) => bsp.run(), // Does not terminate
        Err(e) => panic!("Could not initialize: {:?}", e),
    }
}
