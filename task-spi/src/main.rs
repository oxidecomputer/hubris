#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use cortex_m_semihosting::hprintln;
use userlib::*;

#[cfg(feature = "standalone")]
const SPI: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const SPI: Task = Task::spi_driver;

#[export_name = "main"]
fn main() -> ! {
    let spi = TaskId::for_index_and_gen(SPI as usize, Generation::default());
    hprintln!("Waiting to receive SPI data").ok();
    loop {
        let mut recv: [u8; 4] = [0; 4];
        let b: &mut [u8] = &mut recv;

        cfg_if::cfg_if! {
            if #[cfg(target_board = "gemini-bu-rot-1")] {
                let buf : [u8; 4] = [0xCA, 0xFE, 0xFE, 0xED];
            } else {
                let buf : [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
            }
        }

        let op = 3;
        let a: &[u8] = &buf;
        hprintln!("Starting a new call...").ok();
        let (code, _) =
            sys_send(spi, op, &[], &mut [], &[Lease::from(a), Lease::from(b)]);
        if code != 0 {
            hprintln!("Got error code {}", code).ok();
        } else {
            hprintln!("Got buffer {:x?}", recv).ok();
        }
    }
}
