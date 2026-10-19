// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Periodically reads a 4-byte register from an I2C device.
//!
//! Each poll writes a 4-byte register address then reads back a 4-byte value.
//! Results are recorded in a ringbuf, which can be inspected with `humility
//! ringbuf`.

#![no_std]
#![no_main]

use drv_i2c_api::ResponseCode;
use ringbuf::*;
use userlib::{hl, task_slot};

task_slot!(I2C, i2c_driver);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

/// Register address to read, sent on the wire most-significant byte first.
const REGISTER: u32 = 0x0000_0000;

/// Poll interval, in milliseconds.
const POLL_INTERVAL_MS: u64 = 1000;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Trace {
    None,
    Start,
    Read {
        reg: u32,
        /// Bytes exactly as they arrived on the wire.
        value: [u8; 4],
    },
    ReadFailed {
        reg: u32,
        code: ResponseCode,
    },
}
ringbuf!(Trace, 32, Trace::None);

#[unsafe(export_name = "main")]
fn main() -> ! {
    let [dev] = i2c_config::devices::i2c_poller_target(I2C.get_task_id());

    ringbuf_entry!(Trace::Start);

    loop {
        match dev.read_reg::<[u8; 4], [u8; 4]>(REGISTER.to_be_bytes()) {
            Ok(value) => ringbuf_entry!(Trace::Read {
                reg: REGISTER,
                value
            }),
            Err(code) => ringbuf_entry!(Trace::ReadFailed {
                reg: REGISTER,
                code
            }),
        }

        hl::sleep_for(POLL_INTERVAL_MS);
    }
}
