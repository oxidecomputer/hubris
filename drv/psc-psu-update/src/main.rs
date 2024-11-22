// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_i2c_api::*;
use drv_i2c_devices::mwocp68::{
    Error as Mwocp68Error, Mwocp68, Mwocp68FirmwareRev, UpdateState,
};
use drv_i2c_devices::Validate;
use ringbuf::*;
use static_cell::ClaimOnceCell;
use userlib::*;

use core::ops::Add;

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

static PSU: ClaimOnceCell<[Psu; 6]> = ClaimOnceCell::new(
    [Psu {
        last_checked: None,
        present: None,
        power_good: None,
        firmware_matches: None,
        firmware_revision: None,
        update_started: None,
        update_succeeded: None,
        update_failure: None,
        update_backoff: None,
    }; 6],
);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Start,
    PowerGoodFailed(u8, drv_i2c_devices::mwocp68::Error),
    FirmwareRevFailed(u8, drv_i2c_devices::mwocp68::Error),
    Psu(u8),
    AttemptingUpdate(u8),
    BackingOff(u8),
    UpdateFailed,
    UpdateFailedState(Option<UpdateState>),
    UpdateFailure(Mwocp68Error),
    UpdateState(UpdateState),
    WroteBlock,
    UpdateSucceeded,
    UpdateDelay(u64),
}

const MWOCP68_FIRMWARE_REV: Mwocp68FirmwareRev = Mwocp68FirmwareRev(*b"0762");
const MWOCP68_FIRMWARE_PAYLOAD: &'static [u8] =
    include_bytes!("mwocp68-0762.bin");

ringbuf!(Trace, 32, Trace::None);

#[derive(Copy, Clone, PartialOrd, PartialEq)]
struct Ticks(u64);

impl Ticks {
    fn now() -> Self {
        Self(sys_get_timer().now)
    }
}

impl Add for Ticks {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

#[derive(Copy, Clone, Default)]
struct Psu {
    /// When did we last check this device?
    last_checked: Option<Ticks>,

    /// Is the device physically present?
    present: Option<bool>,

    /// Is the device on and with POWER_GOOD set?
    power_good: Option<bool>,

    /// The last firmware revision read
    firmware_revision: Option<Mwocp68FirmwareRev>,

    /// Does the firmware we have match the firmware here?
    firmware_matches: Option<bool>,

    /// What time did we start an update?
    update_started: Option<Ticks>,

    /// What time did the update complete?
    update_succeeded: Option<Ticks>,

    /// What time did the update last fail, if any?
    update_failure: Option<(Ticks, Option<UpdateState>, Mwocp68Error)>,

    /// How long should the next update backoff, if at all? (In ticks.)
    update_backoff: Option<Ticks>,
}

impl Psu {
    fn update_should_be_attempted(&mut self, dev: &Mwocp68, ndx: u8) -> bool {
        let now = Ticks::now();

        self.last_checked = Some(now);
        self.power_good = None;
        self.firmware_matches = None;
        self.firmware_revision = None;

        if !dev.present() {
            self.present = Some(false);

            //
            // If we are seeing our device as not present, we will clear our
            // backoff value: if/when a PSU is plugged back in, we want to
            // attempt to update it immediately if the firmware revision
            // doesn't match our payload.
            //
            self.update_backoff = None;
            return false;
        }

        self.present = Some(true);

        match dev.power_good() {
            Ok(power_good) => {
                self.power_good = Some(power_good);

                if !power_good {
                    return false;
                }
            }
            Err(err) => {
                ringbuf_entry!(Trace::PowerGoodFailed(ndx, err));
                return false;
            }
        }

        match dev.firmware_revision() {
            Ok(revision) => {
                self.firmware_revision = Some(revision);

                if revision == MWOCP68_FIRMWARE_REV {
                    self.firmware_matches = Some(true);
                    return false;
                }

                self.firmware_matches = Some(false);
            }
            Err(err) => {
                ringbuf_entry!(Trace::FirmwareRevFailed(ndx, err));
                return false;
            }
        }

        if let (Some(started), Some(backoff)) =
            (self.update_started, self.update_backoff)
        {
            if started + backoff > now {
                ringbuf_entry!(Trace::BackingOff(ndx));
                return false;
            }
        }

        true
    }

    fn update_firmware(&mut self, dev: &Mwocp68, ndx: u8) {
        ringbuf_entry!(Trace::AttemptingUpdate(ndx));
        self.update_started = Some(Ticks::now());

        self.update_backoff = match self.update_backoff {
            Some(backoff) => Some(Ticks(backoff.0 * 2)),
            None => Some(Ticks(60_000)),
        };

        let mut state = None;

        loop {
            match dev.update(state, MWOCP68_FIRMWARE_PAYLOAD) {
                Err(err) => {
                    //
                    // We failed.  Record everything we can and leave.
                    //
                    ringbuf_entry!(Trace::UpdateFailed);
                    ringbuf_entry!(Trace::UpdateFailedState(state));
                    ringbuf_entry!(Trace::UpdateFailure(err));

                    self.update_failure = Some((Ticks::now(), state, err));
                    break;
                }

                Ok((UpdateState::UpdateSuccessful, _)) => {
                    ringbuf_entry!(Trace::UpdateSucceeded);
                    self.update_succeeded = Some(Ticks::now());
                    break;
                }

                Ok((next, delay)) => {
                    match next {
                        UpdateState::WroteBlock { .. } => {
                            ringbuf_entry!(Trace::WroteBlock);
                        }
                        _ => {
                            ringbuf_entry!(Trace::UpdateState(next));
                            ringbuf_entry!(Trace::UpdateDelay(delay));
                        }
                    }

                    hl::sleep_for(delay);
                    state = Some(next);
                }
            }
        }
    }
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

            if psu.update_should_be_attempted(dev, ndx as u8) {
                psu.update_firmware(dev, ndx as u8);
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
