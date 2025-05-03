// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for updating all PSUs to the contained binary payload.
//!
//! We have the capacity to dynamically update the MWOCP68 power supply units
//! connected to the PSC.  This update does not involve any interruption of the
//! PSU while it is being performed, but necessitates a reset of the PSU once
//! completed.  We want these updates to be automatic and autonomous; there is
//! little that the control plane can know that we do not know -- and even less
//! for the operator.
//!
//! This task contains within it a payload that is the desired firmware image
//! (`MWOCP68_FIRMWARE_PAYLOAD`), along with the `MFR_REVISION` that that
//! pyaload represents (`MWOCP68_FIRMWARE_VERSION`).  This task will check
//! every PSU periodically to see if the PSU's firmware revision matches the
//! revision specified as corresponding to the payload; if they don't match (or
//! rather, until they do), an attempt will be made to update the PSU.  Each
//! PSU will be updated sequentially: while we can expect a properly configured
//! and operating rack to support the loss of any one PSU, we do not want to
//! induce the loss of more than one simultaneously due to update.  If an
//! update fails, the update of that PSU will be exponentially backed off and
//! repeated (up to a backoff of about once per day).  Note that we will
//! continue to check PSUs that we have already updated should they be replaced
//! with a PSU with downrev firmware.  The state of this task can be
//! ascertained by looking at the `PSU` variable (which contains all of the
//! per-PSU state) as well as the ring buffer.
//!

#![no_std]
#![no_main]

use drv_i2c_api::*;
use drv_i2c_devices::mwocp68::{
    Error as Mwocp68Error, FirmwareRev, Mwocp68, SerialNumber, UpdateState,
};
use ringbuf::*;
use static_cell::ClaimOnceCell;
use userlib::*;

use core::ops::Add;

task_slot!(I2C, i2c_driver);

const TIMER_INTERVAL_MS: u64 = 10_000;

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
        serial_number: None,
        firmware_matches: None,
        firmware_revision: None,
        update_started: None,
        update_succeeded: None,
        update_failure: None,
        update_backoff: None,
    }; 6],
);

#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    PowerGoodFailed(u8, drv_i2c_devices::mwocp68::Error),
    FirmwareRevFailed(u8, drv_i2c_devices::mwocp68::Error),
    AttemptingUpdate(u8),
    BackingOff(u8),
    UpdateFailed,
    UpdateFailedState(Option<UpdateState>),
    UpdateFailure(Mwocp68Error),
    UpdateState(UpdateState),
    WroteBlock,
    UpdateSucceeded(u8),
    UpdateDelay(u64),
    PSUReplaced(u8),
    SerialNumberError(u8, drv_i2c_devices::mwocp68::Error),
    PGError(u8, drv_i2c_devices::mwocp68::Error),
    PowerNotGood(u8),
}

//
// The actual firmware revision and payload. It is very important that the
// revision match the revision contained within the payload, lest we will
// believe that the update has failed when it has in fact succeeded!
//
const MWOCP68_FIRMWARE_REV: FirmwareRev = FirmwareRev(*b"0762");
const MWOCP68_FIRMWARE_PAYLOAD: &[u8] = include_bytes!("mwocp68-0762.bin");

counted_ringbuf!(Trace, 64, Trace::None);

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

    /// The last serial number read
    serial_number: Option<SerialNumber>,

    /// The last firmware revision read
    firmware_revision: Option<FirmwareRev>,

    /// Does the firmware we have match the firmware here?
    firmware_matches: Option<bool>,

    /// What time did we start an update?
    update_started: Option<Ticks>,

    /// What time did the update complete?
    update_succeeded: Option<Ticks>,

    /// What time did the update last fail, if any?
    update_failure: Option<(Ticks, Option<UpdateState>, Option<Mwocp68Error>)>,

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

        //
        // If we can read the serial number, we're going to store it -- and
        // if we previously stored one and it DOESN'T match, we want to
        // clear our backoff value so we don't delay at all in potentially
        // trying to update the firmware of the (replaced) PSU.  (If we can't
        // read the serial number at all, we want to continue to potentially
        // update the firmware.)
        //
        match (dev.serial_number(), self.serial_number) {
            (Ok(read), Some(stored)) if read != stored => {
                ringbuf_entry!(Trace::PSUReplaced(ndx));
                self.update_backoff = None;
                self.serial_number = Some(read);
            }
            (Ok(_), Some(_)) => {}
            (Ok(read), None) => {
                self.serial_number = Some(read);
            }
            (Err(code), _) => {
                ringbuf_entry!(Trace::SerialNumberError(ndx, code));
            }
        }

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
                //
                // Indicate we are backing off, but in a way that won't flood
                // the ring buffer with the backing off of a single PSU.
                //
                ringbuf_entry!(Trace::BackingOff(ndx));
                return false;
            }
        }

        true
    }

    fn update_firmware(&mut self, dev: &Mwocp68, ndx: u8) {
        ringbuf_entry!(Trace::AttemptingUpdate(ndx));
        self.update_started = Some(Ticks::now());

        //
        // Before we start, update our backoff.  We'll double our backoff, up
        // to a cap of around a day.
        //
        self.update_backoff = match self.update_backoff {
            Some(backoff) if backoff.0 < 86_400_000 => {
                Some(Ticks(backoff.0 * 2))
            }
            Some(backoff) => Some(backoff),
            None => Some(Ticks(75_000)),
        };

        let mut state = None;

        let mut update_failed = |state, err| {
            //
            // We failed.  Record everything we can!
            //
            if let Some(err) = err {
                ringbuf_entry!(Trace::UpdateFailure(err));
            }

            ringbuf_entry!(Trace::UpdateFailed);
            ringbuf_entry!(Trace::UpdateFailedState(state));
            self.update_failure = Some((Ticks::now(), state, err));
        };

        loop {
            match dev.update(state, MWOCP68_FIRMWARE_PAYLOAD) {
                Err(err) => {
                    update_failed(state, Some(err));
                    break;
                }

                Ok((UpdateState::UpdateSuccessful, _)) => {
                    let state = Some(UpdateState::UpdateSuccessful);

                    //
                    // We should be back up!  As a final measure, we are going
                    // to check that the firmware revision matches the
                    // revision we think we just wrote.  If it doesn't, there
                    // is something amiss:  it may be that the image is
                    // corrupt or that the version doesn't otherwise match.
                    // Regardless, we consider that to be an update failure.
                    //
                    match dev.firmware_revision() {
                        Ok(revision) if revision != MWOCP68_FIRMWARE_REV => {
                            update_failed(state, None);
                            break;
                        }

                        Err(err) => {
                            update_failed(state, Some(err));
                            break;
                        }

                        Ok(_) => {}
                    }

                    //
                    // We're on the new firmware!  And now, a final final
                    // check: make sure that we are power-good.  It is very
                    // unclear what to do here if are NOT power-good:  we know
                    // that we WERE power-good before we started, so it
                    // certainly seems possible that we have put a firmware
                    // update on this PSU which has somehow incapacitated it.
                    // We would rather not put the system in a compromised
                    // state by continuing to potentially brick PSUs -- but we
                    // also want to assure that we make progress should this
                    // ever resolve (e.g., by pulling the bricked PSU). We will
                    // remain here until we see the updated PSU go power-good;
                    // if it never does, we will at least not attempt to put
                    // the (potentially) bad update anywhere else!
                    //
                    loop {
                        match dev.power_good() {
                            Ok(power_good) if power_good => break,
                            Ok(_) => {
                                ringbuf_entry!(Trace::PowerNotGood(ndx));
                            }
                            Err(err) => {
                                ringbuf_entry!(Trace::PGError(ndx, err));
                            }
                        }

                        hl::sleep_for(TIMER_INTERVAL_MS);
                    }

                    ringbuf_entry!(Trace::UpdateSucceeded(ndx));
                    self.update_succeeded = Some(Ticks::now());
                    self.update_backoff = None;
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

    let psus = PSU.claim();

    let devs: [Mwocp68; 6] = array_init::array_init(|ndx: usize| {
        Mwocp68::new(&DEVICES[ndx](i2c_task), 0)
    });

    loop {
        hl::sleep_for(TIMER_INTERVAL_MS);

        for (ndx, psu) in psus.iter_mut().enumerate() {
            let dev = &devs[ndx];

            if psu.update_should_be_attempted(dev, ndx as u8) {
                psu.update_firmware(dev, ndx as u8);
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
