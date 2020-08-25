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
    let reg: &[u8] = &[0x0b];
    let i2c = TaskId::for_index_and_gen(I2C as usize, Generation::default());

    hprintln!("In AD7420 task").ok();
    let mut target: u8 = 0x48;
    let zero: &[u8] = &[0u8; 0];

    loop {
        let mut recv: [u8; 1] = [0; 1];
        let a: &mut [u8] = &mut recv;
        let bufs = &[Lease::from(reg), Lease::from(a)];

        let (code, _) = sys_send(i2c, 1, &[target], &mut [], bufs);

        if code != 0 {
            hprintln!("0x{:x}: Error code: {}", target, code).ok();
        } else {
            hprintln!("!!! Got buffer {:x?}", recv).ok();
        }

        let mut temp: [u8; 2] = [0; 2];
        let t: &mut [u8] = &mut temp;
        let bufs = &[Lease::from(zero), Lease::from(t)];

        let (code, _) = sys_send(i2c, 1, &[target], &mut [], bufs);

        if code != 0 {
            hprintln!("0x{:x}: Error code: {}", target, code).ok();
        } else {
            hprintln!("!!! Got buffer {:x?}", temp).ok();
        }
    }
}
