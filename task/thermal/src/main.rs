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
use drv_i2c_devices::sbtsi::*;
use drv_i2c_devices::tmp116::*;
use drv_i2c_devices::TempSensor;
use userlib::units::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;

enum Zone {
    East,
    Central,
    West,
}

enum Sensor {
    North(Zone, Tmp116),
    South(Zone, Tmp116),
    CPU(SbTsi),
}

fn temp_read<E: core::fmt::Debug, T: TempSensor<E> + core::fmt::Display>(
    device: &T,
) -> Option<Celsius> {
    match device.read_temperature() {
        Ok(reading) => Some(reading),

        Err(err) => {
            sys_log!("{}: failed to read temp: {:?}", device, err);
            None
        }
    }
}

impl core::fmt::Display for Sensor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Sensor::North(zone, _) => match zone {
                    Zone::East => "northeast",
                    Zone::Central => "north",
                    Zone::West => "northwest",
                },
                Sensor::South(zone, _) => match zone {
                    Zone::East => "southeast",
                    Zone::Central => "south",
                    Zone::West => "southwest",
                },
                Sensor::CPU(_) => "sbtsi",
            }
        )
    }
}

impl Sensor {
    fn read_temp(&self) -> Option<Celsius> {
        match self {
            Sensor::North(_, dev) | Sensor::South(_, dev) => temp_read(dev),
            Sensor::CPU(dev) => temp_read(dev),
        }
    }

    fn log(&self, temp: Celsius) {
        sys_log!(
            "{}: {}.{:03}C",
            self,
            temp.0 as i32,
            (((temp.0 + 0.0005) * 1000.0) as i32) % 1000,
        )
    }
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

fn sensors() -> [Sensor; 7] {
    let task = I2C.get_task_id();

    [
        Sensor::North(
            Zone::East,
            Tmp116::new(&devices::tmp117_rear_zone1(task)),
        ),
        Sensor::North(
            Zone::Central,
            Tmp116::new(&devices::tmp117_rear_zone2(task)),
        ),
        Sensor::North(
            Zone::West,
            Tmp116::new(&devices::tmp117_rear_zone3(task)),
        ),
        Sensor::South(
            Zone::East,
            Tmp116::new(&devices::tmp117_front_zone1(task)),
        ),
        Sensor::South(
            Zone::Central,
            Tmp116::new(&devices::tmp117_front_zone2(task)),
        ),
        Sensor::South(
            Zone::West,
            Tmp116::new(&devices::tmp117_front_zone3(task)),
        ),
        Sensor::CPU(SbTsi::new(&devices::sbtsi(task)[0])),
    ]
}

#[export_name = "main"]
fn main() -> ! {
    let sensors = sensors();
    let task = I2C.get_task_id();

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gimlet-1")] {
            let fctrl = Max31790::new(&devices::max31790(task)[0]);
        } else if #[cfg(feature = "standalone")] {
            let fctrl = Max31790::new(&devices::mock(task));
        } else {
            compile_error!("unknown board");
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
        for s in &sensors {
            if let Some(temp) = s.read_temp() {
                s.log(temp);
            }
        }

        read_fans(&fctrl);

        hl::sleep_for(1000);
    }
}
