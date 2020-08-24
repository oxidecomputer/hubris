#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use cortex_m_semihosting::hprintln;
use userlib::*;

#[cfg(feature = "standalone")]
const I2C: Task = SELF;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

#[export_name = "main"]
fn main() -> ! {
    // let addr: &[u8] = &[0xa8];
    // let addr: &[u8] = &[0x0b];
    let addr: &[u8] = &[0x0b];
    let i2c = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    hprintln!("In AD7420 task").ok();
    let mut target: u8 = 0x48;

    loop {
        let mut recv: [u8; 4] = [0; 4];
        let a: &mut [u8] = &mut recv;

        let (code, _) =
            sys_send(i2c, 1, &[target], &mut [], &[Lease::from(addr)]);
        if code != 0 {
            hprintln!("0x{:x}: Error code: {}", target, code).ok();
        } else {
            hprintln!("!!! 0x{:x}: Success!", target).ok();
        }

        let (code, _) = sys_send(i2c, 2, &[target], &mut [], &[Lease::from(a)]);
        if code != 0 {
            hprintln!("Got error code{}", code).ok();
        } else {
            hprintln!("!!! Got buffer {:x?}", recv).ok();
        }
    }
}
