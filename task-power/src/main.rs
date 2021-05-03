//! Power monitoring
//!
//! This is a primordial power monitoring task.
//!

#![no_std]
#![no_main]

use drv_i2c_api::*;
use drv_i2c_devices::adm1272::*;
use userlib::units::*;
use userlib::*;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

#[cfg(feature = "standalone")]
const I2C: Task = Task::anonymous;

#[export_name = "main"]
fn main() -> ! {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            const ADM1272_ADDRESS: u8 = 0x10;

            let adm1272 = Adm1272::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::H,
                None,
                ADM1272_ADDRESS
            ));
        } else {
            cfg_if::cfg_if! {
                if #[cfg(feature = "standalone")] {
                    let device = I2cDevice::mock(task);
                    let adm1272 = Adm1272::new(&device);
                } else {
                    compile_error!("unknown board");
                }
            }
        }
    }

    loop {
        let mut buf = [0u8; 128];

        match adm1272.read_model(&mut buf) {
            Ok(_) => {
                if let Ok(idstr) = core::str::from_utf8(&buf) {
                    sys_log!("{}: {}", adm1272, idstr);
                } else {
                    sys_log!("{}: {:x}", adm1272, buf[0]);
                }
            }
            Err(err) => {
                sys_log!("{}: initialization failed: {:?}", adm1272, err);
            }
        }

        hl::sleep_for(1000);
    }
}
