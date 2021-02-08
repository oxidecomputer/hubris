#![no_std]
#![no_main]

use userlib::*;
use drv_i2c_api::*;

mod max31790;
use max31790::*;

#[cfg(feature = "standalone")]
const I2C: Task = SELF;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

#[no_mangle]
static mut MAX31790_FAN_RPM: [Option<Result<(Fan, u16), Error>>; 6] = [None; 6];

#[export_name = "main"]
fn main() -> ! {
    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            const MAX31790_ADDRESS: u8 = 0x20;

            let i2c = I2c::new(
                TaskId::for_index_and_gen(I2C as usize, Generation::default()),
                Controller::I2C1,
                Port::Default,
                None,
                MAX31790_ADDRESS,
            );
        } else {
            compile_error!("unknown board");
        }
    }

    loop {
        match max31790::initialize(&i2c) {
            Ok(_) => {
                sys_log!("initialization successful!");
                break;
            }
            Err(err) => {
                sys_log!("initialization failed!");
                hl::sleep_for(1000);
            }
        }
    }

    loop {
        let mut rpm = unsafe { &mut MAX31790_FAN_RPM };
        let mut ndx = 0;

        for fan in FAN_MIN..=FAN_MAX {
            let fan = Fan(fan);

            rpm[ndx] = Some(match fan_rpm(&i2c, fan) {
                Ok(rval) => Ok((fan, rval)),
                Err(err) => Err(err)
            });

            ndx = ndx + 1;
        }

        hl::sleep_for(1000);
    }
}
