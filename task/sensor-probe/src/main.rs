// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Reads every sensor once a second.
//!
//! A simulation helper: it does nothing with the readings, but its requests
//! make every sensor's current value visible in a trace of the `sensor`
//! task, alongside the posts that set them.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use task_sensor_api::{Sensor, SensorId, config::NUM_SENSORS};
use userlib::{set_timer_relative, sys_recv_notification, task_slot};

task_slot!(SENSOR, sensor);

/// Ticks between rounds of reads.
const INTERVAL: u32 = 1000;

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    let sensor = Sensor::from(SENSOR.get_task_id());
    loop {
        set_timer_relative(INTERVAL, notifications::TIMER_MASK);
        sys_recv_notification(notifications::TIMER_MASK);
        for id in 0..NUM_SENSORS as u32 {
            // A sensor with no reading yet answers with an error; that is
            // just as informative in the trace as a value.
            let _ = sensor.get_reading(SensorId::new(id));
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
