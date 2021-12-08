#![no_std]
#![no_main]

// NOTE: you will probably want to remove this when you write your actual code;
// we need to import userlib to get this to compile, but it throws a warning
// because we're not actually using it yet!
#[allow(unused_imports)]
use userlib::*;

use drv_spi_api as spi_api;

const VSC7448_SPI_DEVICE: u8 = 0;
task_slot!(SPI, spi_driver);

#[export_name = "main"]
fn main() -> ! {
    let spi = spi_api::Spi::from(SPI.get_task_id());

    let spi = spi.device(VSC7448_SPI_DEVICE);
    loop {
        hl::sleep_for(10);
    }
}
