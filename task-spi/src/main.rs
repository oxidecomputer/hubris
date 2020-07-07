#![no_std]
#![no_main]
#![feature(llvm_asm)]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use cortex_m_semihosting::hprintln;
use userlib::*;

#[cfg(feature = "standalone")]
const SPI: Task = SELF;

#[cfg(not(feature = "standalone"))]
const SPI: Task = Task::spi_driver;

#[export_name = "main"]
fn main() -> ! {
    let spi = TaskId::for_index_and_gen(SPI as usize, Generation::default());
    hprintln!("Waiting to receive SPI data").ok();
    loop {
        let mut recv: [u8; 5] = [0; 5];
        let a: &mut [u8] = &mut recv;
        let (code, _) = sys_send(spi, 2, &[], &mut [], &[Lease::from(a)]);
        if code != 0 {
            hprintln!("Got error code {}", code).ok();
        } else {
            hprintln!("Got buffer {:x?}", recv).ok();
        }
    }
}
