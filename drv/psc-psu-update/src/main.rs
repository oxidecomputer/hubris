// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_i2c_devices::mwocp68::{Error as Mwocp68Error, Mwocp68};
use ringbuf::*;
use userlib::*;
use drv_i2c_api::*;
use drv_i2c_devices::Validate;
use static_cell::ClaimOnceCell;

task_slot!(I2C, i2c_driver);

const TIMER_INTERVAL: u64 = 10000;

use i2c_config::devices;

#[cfg(any(target_board = "psc-b", target_board = "psc-c"))]
static DEVICES: [fn(TaskId) -> I2cDevice; 6] = [
    devices::mwocp68_psu0mcu,
    devices::mwocp68_psu1mcu,
    devices::mwocp68_psu2mcu,
    devices::mwocp68_psu3mcu,
    devices::mwocp68_psu4mcu,
    devices::mwocp68_psu5mcu,
];

static PSU: ClaimOnceCell<[Psu; 6]> = ClaimOnceCell::new([Psu {
    last_checked: None,
    present: None,
    power_good: None,
    firmware_matches: None,
    update_started: None,
    update_failed: None,
}; 6]);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Trace {
    None,
    Start,
    PowerGoodFailed(u8, drv_i2c_devices::mwocp68::Error),
    Psu(u8),
}

ringbuf!(Trace, 32, Trace::None);

#[derive(Copy, Clone)]
struct Ticks(u64);

#[derive(Copy, Clone)]
struct Psu {
    /// When did we last check this device?
    last_checked: Option<Ticks>,

    /// Is the device physically present?
    present: Option<bool>,

    /// Is the device on and with POWER_GOOD set?
    power_good: Option<bool>,

//    firmware_rev: Option<Mwocp68FirmwareRev>

    /// Does the firmware we have match the firmware here?
    firmware_matches: Option<bool>,

    /// What time did we start an update?
    update_started: Option<Ticks>,

    /// What time did the update last fail, if any?
    update_failed: Option<Ticks>,
}

#[export_name = "main"]
fn main() -> ! {
    let i2c_task = I2C.get_task_id();

    ringbuf_entry!(Trace::Start);

    let mut psus = PSU.claim();

    let devs: [Mwocp68; 6] = array_init::array_init(|ndx: usize| {
        Mwocp68::new(&DEVICES[ndx](i2c_task), 0)
    });

    loop {
        hl::sleep_for(TIMER_INTERVAL);

        for (ndx, psu) in psus.iter_mut().enumerate() {
            let dev = &devs[ndx];

            psu.last_checked = Some(Ticks(sys_get_timer().now));

            //
            // We're going to check all of these fields.
            //
            psu.power_good = None;
            psu.firmware_matches = None;

            if !dev.present() {
                psu.present = Some(false);
                continue;
            }

            psu.present = Some(true);

            match dev.power_good() {
                Ok(power_good) => {
                    psu.power_good = Some(power_good);

                    if !power_good { 
                        continue;
                    }
                }

                Err(err) => {
                    ringbuf_entry!(Trace::PowerGoodFailed(ndx as u8, err));
                    continue;
                }
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
