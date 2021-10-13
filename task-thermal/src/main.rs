//! Thermal loop
//!
//! This is a primordial thermal loop, which will ultimately reading temperature
//! sensors and control fan duty cycles to actively manage thermals.  Right now,
//! though it is merely reading every fan and temp sensor that it can find...
//!

#![no_std]
#![no_main]

use drv_i2c_api::*;
use drv_i2c_devices::max31790::*;
use drv_i2c_devices::tmp116::*;
use drv_i2c_devices::TempSensor;
use userlib::units::*;
use userlib::*;

declare_task!(I2C, i2c_driver);

fn convert_fahrenheit(temp: Celsius) -> f32 {
    temp.0 * (9.0 / 5.0) + 32.0
}

fn print_temp<T: core::fmt::Display>(temp: Celsius, device: &T) {
    let f = convert_fahrenheit(temp);

    sys_log!(
        "{}: temp is {}.{:03} degrees C, {}.{:03} degrees F",
        device,
        temp.0 as i32,
        (((temp.0 + 0.0005) * 1000.0) as i32) % 1000,
        f as i32,
        (((f + 0.0005) * 1000.0) as i32) % 1000
    );
}

fn read_fans(fctrl: &Max31790) {
    let mut ndx = 0;

    for fan in 0..MAX_FANS {
        let fan = Fan::new(fan).unwrap();

        match fctrl.fan_rpm(fan) {
            Ok(rval) if rval.0 != 0 => {
                sys_log!("{}: {}: RPM={}", fctrl, fan, rval.0);
            }
            Ok(_) => {}
            Err(err) => {
                sys_log!("{}: {}: failed: {:?}", fctrl, fan, err);
            }
        }

        ndx = ndx + 1;
    }
}

fn temp_read<E: core::fmt::Debug, T: TempSensor<E> + core::fmt::Display>(
    device: &T,
) {
    match device.read_temperature() {
        Ok(temp) => {
            print_temp(temp, device);
        }

        Err(err) => {
            sys_log!("{}: failed to read temp: {:?}", device, err);
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let task = get_task_id(I2C);

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            const MAX31790_ADDRESS: u8 = 0x20;

            let fctrl = Max31790::new(&I2cDevice::new(
                task,
                Controller::I2C1,
                Port::Default,
                None,
                MAX31790_ADDRESS,
            ));

            let tmp116: [Tmp116; 0] = [];
        } else if #[cfg(target_board = "gimlet-1")] {
            // Two sets of TMP117 sensors, Front and Rear
            // These all have the same address but are on different
            // controllers/ports

            const TMP116_ADDRESS: u8 = 0x48;

            // Front sensors (U.2)
            let tmp116 = [ Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C2,
                Port::F,
                None,
                TMP116_ADDRESS
            )), Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C2,
                Port::F,
                None,
                TMP116_ADDRESS + 1
            )),
                Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C2,
                Port::F,
                None,
                TMP116_ADDRESS + 2
            )),

            // Rear sensors (fans)
                Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                None,
                TMP116_ADDRESS
            )), Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                None,
                TMP116_ADDRESS + 1
            )),
                Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                None,
                TMP116_ADDRESS + 2
            )),
            ];

            const MAX31790_ADDRESS: u8 = 0x20;

            let fctrl = Max31790::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                None,
                MAX31790_ADDRESS,
            ));
        } else {
            cfg_if::cfg_if! {
                if #[cfg(feature = "standalone")] {
                    let device = I2cDevice::mock(task);
                    let fctrl = Max31790::new(&device);
                    let tmp116 = [ Tmp116::new(&device) ];
                } else {
                    compile_error!("unknown board");
                }
            }
        }
    }

    loop {
        match fctrl.initialize() {
            Ok(_) => {
                sys_log!("{}: initialization successful", fctrl);
                break;
            }
            Err(err) => {
                sys_log!("{}: initialization failed: {:?}", fctrl, err);
                hl::sleep_for(1000);
            }
        }
    }

    loop {
        read_fans(&fctrl);

        for device in &tmp116 {
            temp_read(device);
        }

        hl::sleep_for(1000);
    }
}
