#![no_std]
#![no_main]

use userlib::*;
use userlib::units::Celsius;
use drv_i2c_api::*;
use drv_i2c_devices::adt7420::*;

#[cfg(feature = "standalone")]
const I2C: Task = SELF;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

const ADT7420_ADDRESS: u8 = 0x48;

fn convert_fahrenheit(temp: Celsius) -> f32 {
    temp.0 * (9.0 / 5.0) + 32.0
}

#[export_name = "main"]
fn main() -> ! {
    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            let mut devices = [ (Adt7420::new(&I2c::new(
                TaskId::for_index_and_gen(I2C as usize, Generation::default()),
                Controller::I2C4,
                Port::F,
                Some((Mux::M1, Segment::S1)),
                ADT7420_ADDRESS
            )), false), (Adt7420::new(&I2c::new(
                TaskId::for_index_and_gen(I2C as usize, Generation::default()),
                Controller::I2C4,
                Port::F,
                Some((Mux::M1, Segment::S4)),
                ADT7420_ADDRESS
            )), false)];
        } else {
            compile_error!("unknown board");
        }
    }

    loop {
        hl::sleep_for(1000);

        for (device, validated) in &mut devices {
            if *validated {
                let temp = match device.read_temperature() {
                    Ok(temp) => temp,
                    Err(err) => {
                        sys_log!("{}: failed to read temp: {:?}", device, err);
                        continue;
                    }
                };

                let f = convert_fahrenheit(temp);

                // Avoid default formatting to save a bunch of text and stack
                sys_log!("{}: temp is {}.{:03} degrees C, \
                    {}.{:03} degrees F",
                    device,
                    temp.0 as i32, (((temp.0 + 0.0005) * 1000.0) as i32) % 1000,
                    f as i32, (((f + 0.0005) * 1000.0) as i32) % 1000);
            } else {
                match device.validate() {
                    Ok(_) => {
                        sys_log!("{}: found device!", device);
                        *validated = true;
                    }
                    Err(err) => {
                        sys_log!("{}: no bueno: {:?}", device, err);
                    }
                }
            }
        }
    }
}
