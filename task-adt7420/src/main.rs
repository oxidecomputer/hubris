#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use cortex_m_semihosting::hprintln;
use userlib::*;
use drv_i2c_api::*;

#[cfg(feature = "standalone")]
const I2C: Task = SELF;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

#[export_name = "main"]
fn main() -> ! {
    let i2c = I2c::from(TaskId::for_index_and_gen(
        I2C as usize,
        Generation::default()
    ));

    hprintln!("In AD7420 task").ok();

    loop {
        match i2c.read_reg::<u8, u8>(Interface::I2C4, 0x48, 0xb) {
            Ok(val) => { hprintln!("value is {:x}", val).ok(); }
            Err(err) => { hprintln!("failed: {:?}", err).ok(); }
        }
    }
}
