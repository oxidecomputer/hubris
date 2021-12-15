// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Thermal loop
//!
//! This is a primordial thermal loop, which will ultimately reading temperature
//! sensors and control fan duty cycles to actively manage thermals.  Right now,
//! though it is merely reading every fan and temp sensor that it can find...
//!

#![no_std]
#![no_main]

use drv_i2c_devices::max31790::*;
use drv_i2c_devices::tmp116::*;
use drv_i2c_devices::TempSensor;
use userlib::units::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

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
    let task = I2C.get_task_id();
    use i2c_config::devices;

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            let fctrl = Max31790::new(&devices::max31790(task)[0]);
            let tmp116: [Tmp116; 0] = [];
        } else if #[cfg(target_board = "gimlet-1")] {
            let tmp116 = [
                Tmp116::new(&devices::tmp117_front_zone1(task)),
                Tmp116::new(&devices::tmp117_front_zone2(task)),
                Tmp116::new(&devices::tmp117_front_zone3(task)),
                Tmp116::new(&devices::tmp117_rear_zone1(task)),
                Tmp116::new(&devices::tmp117_rear_zone2(task)),
                Tmp116::new(&devices::tmp117_rear_zone3(task)),
            ];

            let fctrl = Max31790::new(&devices::max31790(task)[0]);
        } else {
            cfg_if::cfg_if! {
                if #[cfg(feature = "standalone")] {
                    let fctrl = Max31790::new(&devices::mock(task));
                    let tmp116: [Tmp116; 0] = [];
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
