#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use userlib::*;

#[cfg(feature = "standalone")]
const SPI: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const SPI: Task = Task::spi_driver;

#[export_name = "main"]
fn main() -> ! {
    let spi = TaskId::for_index_and_gen(SPI as usize, Generation::default());
    defmt::debug!("Waiting to receive SPI data");
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
            defmt::debug!("Got error code {:u32}", code);
        } else {
            defmt::debug!("Got buffer {:[u8; 5]}", recv);
        }
    }
}
