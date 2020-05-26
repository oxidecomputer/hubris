#![no_std]
#![no_main]
#![feature(llvm_asm)]

use userlib::*;
use cortex_m_semihosting::hprintln;

#[cfg(not(feature = "standalone"))]
const SPI: Task = Task::spi_driver;


#[export_name = "main"]
fn main() -> ! {
    let test_send: [u8; 16] = [0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8,
    0x9, 0xa, 0xb, 0xc, 0xd, 0xe, 0xf];


    hprintln!("Starting SPI loopback");
    loop {
        let spi = TaskId::for_index_and_gen(SPI as usize, Generation::default());

        for i in test_send.iter() {
            let send : &[u8]= &[*i];
            let mut recv = [0];
            let a : &mut [u8] = &mut recv;
            let (code, _len) = sys_send(spi, 1, &[], &mut [], &[Lease::from(send)]);
            if code != 0 {
                hprintln!("Failed to send {}", code).ok();
            }
            let (code, _len) = sys_send(spi, 2, &[], &mut [], &[Lease::from(a)]);
            if code == 0 {
                hprintln!("sent {:?} got {:?}", send, recv).ok();
            } else {
                hprintln!("Failed to receive {}", code).ok();
            }
        }
    }
}
