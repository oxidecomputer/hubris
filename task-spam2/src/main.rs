#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use defmt;
use userlib::*;

#[cfg(feature = "standalone")]
const I2C: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

#[export_name = "main"]
fn main() -> ! {
    let addr: &[u8] = &[0x0];
    let i2c = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    defmt::debug!("Starting to spam!");
    loop {
        let mut recv: [u8; 4] = [0; 4];
        let a: &mut [u8] = &mut recv;
        // This is address of the WM8904 on Flexcomm 4
        // register 0 = id register that should always return 8904 on read
        let (code, _) =
            sys_send(i2c, 1, &[0x1a], &mut [], &[Lease::from(addr)]);
        if code != 0 {
            defmt::error!("Got error code: {:u32}", code);
        } else {
            defmt::debug!("Success");
        }
        let (code, _) = sys_send(i2c, 2, &[0x1a], &mut [], &[Lease::from(a)]);
        if code != 0 {
            defmt::error!("Got error code: {:u32}", code);
        } else {
            defmt::debug!("Success");
        }
    }
}
