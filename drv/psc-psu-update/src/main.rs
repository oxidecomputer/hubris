// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for updating all PSUs to the contained binary payload.
//!
//! We have the capacity to dynamically update the MWOCP6X power supply units
//! connected to the PSC.  This update does not involve any interruption of the
//! PSU while it is being performed, but necessitates a reset of the PSU once
//! completed.  We want these updates to be automatic and autonomous; there is
//! little that the control plane can know that we do not know -- and even less
//! for the operator.
//!
//! This task's `bsp` module defines `MWOCP6X_PRIMARY_FIRMWARE` and
//! `MWOCP6X_SECONDARY_FIRMWARE` for each board type, which give the desired
//! firmware image for the PSU's primary and secondary MCUs (or `None`, if we
//! choose not to control what firmware is on that MCU).
//!
//! This task will check every PSU periodically to see if the PSU's firmware
//! revision matches the desired revision; if they don't match (or
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

use drv_i2c_devices::mwocp6x::{
    Error as Mwocp6xError, FirmwareImage, FirmwareRev, PsuMcu, SerialNumber,
};

use heapless::Vec;
use ringbuf::*;
use static_cell::ClaimOnceCell;
use userlib::{hl, sys_get_timer, task_slot};

use core::ops::Add;

// Board-specific behavior is isolated into a `bsp` module, which is picked
// based on the target_board name.
#[cfg_attr(
    any(target_board = "psc-b", target_board = "psc-c"),
    path = "bsp/psc_bc.rs"
)]
#[cfg_attr(target_board = "observer-a", path = "bsp/observer_a.rs")]
mod bsp;

task_slot!(I2C, i2c_driver);

const TIMER_INTERVAL_MS: u64 = 10_000;

// The per-PSU signal definitions in the bsp modules all refer to this constant
// for the number of PSUs. It's not intended to be easily configurable, since
// that'd require hardware changes.
pub const PSU_COUNT: usize = 6;

static PSU: ClaimOnceCell<[Psu; PSU_COUNT]> = ClaimOnceCell::new(
    [Psu {
        last_checked: None,
        present: None,
        serial_number: None,
        firmware_revision: None,
        primary: UpdateStatus::new(),
        secondary: UpdateStatus::new(),
    }; PSU_COUNT],
);

#[derive(Copy, Clone, Debug, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    FirmwareRevFailed(u8, Mwocp6xError),
    AttemptingUpdate(u8, PsuMcu),
    BackingOff(u8, PsuMcu),
    UpdateFailed(u8, PsuMcu),
    UpdateFailedState(Option<bsp::UpdateState>),
    UpdateFailure(Mwocp6xError),
    UpdateState(bsp::UpdateState),
    WroteBlock,
    UpdateSucceeded(u8, PsuMcu),
    UpdateDelay(u64),
    PSUReplaced(u8),
    SerialNumberError(u8, Mwocp6xError),
    PowerGoodBefore(u8),
    PowerNotGoodBefore(u8),
    PGErrorBefore(u8, Mwocp6xError),
    PowerGoodAfter(u8),
    PowerNotGoodAfter(u8),
    PGErrorAfter(u8, Mwocp6xError),
}

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

#[derive(Copy, Clone)]
struct Psu {
    /// When did we last check this device?
    last_checked: Option<Ticks>,

    /// Is the device physically present?
    present: Option<bool>,

    /// The last serial number read
    serial_number: Option<SerialNumber>,

    /// The last firmware revision read
    firmware_revision: Option<FirmwareRev>,

    /// The status of our attempts to update the primary MCU
    primary: UpdateStatus,

    /// The status of our attempts to update the secondary MCU
    secondary: UpdateStatus,
}

impl Psu {
    fn status(&mut self, mcu: PsuMcu) -> &mut UpdateStatus {
        match mcu {
            PsuMcu::Primary => &mut self.primary,
            PsuMcu::Secondary => &mut self.secondary,
        }
    }

    /// Clear every MCU's backoff, so that a mismatched firmware revision will
    /// make us immediately retry the update.
    fn clear_backoff(&mut self) {
        self.primary.update_backoff = None;
        self.secondary.update_backoff = None;
    }
}

#[derive(Copy, Clone)]
struct UpdateStatus {
    /// What time did we start an update?
    update_started: Option<Ticks>,

    /// What time did the update complete?
    update_succeeded: Option<Ticks>,

    /// What time did the update last fail, if any?
    update_failure:
        Option<(Ticks, Option<bsp::UpdateState>, Option<Mwocp6xError>)>,

    /// How long should we wait before retrying the update, if we should wait at all?
    update_backoff: Option<Ticks>,
}

impl UpdateStatus {
    const fn new() -> UpdateStatus {
        UpdateStatus {
            update_started: None,
            update_succeeded: None,
            update_failure: None,
            update_backoff: None,
        }
    }

    fn is_backoff_elapsed(&self, now: Ticks) -> bool {
        if let (Some(started), Some(backoff)) =
            (self.update_started, self.update_backoff)
        {
            started + backoff < now
        } else {
            true
        }
    }
}

impl Psu {
    /// Returns a list of firmware images that we should try to install on this
    /// PSU's MCUs right now.
    fn updates_to_attempt(
        &mut self,
        dev: &bsp::Mwocp6x,
        ndx: u8,
    ) -> Vec<FirmwareImage, { PsuMcu::COUNT }> {
        let now = Ticks::now();

        self.last_checked = Some(now);
        self.firmware_revision = None;

        if !dev.present() {
            self.present = Some(false);

            //
            // If we are seeing our device as not present, we will clear our
            // backoff values: if/when a PSU is plugged back in, we want to
            // attempt to update it immediately if the firmware revision
            // doesn't match our payload.
            //
            self.clear_backoff();
            return Vec::new();
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
                self.clear_backoff();
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

        match dev.firmware_revision() {
            Err(err) => {
                ringbuf_entry!(Trace::FirmwareRevFailed(ndx, err));
                Vec::new()
            }
            Ok(revision) => {
                self.firmware_revision = Some(revision);
                let mut to_update = Vec::new();
                for firmware in [
                    bsp::MWOCP6X_PRIMARY_FIRMWARE,
                    bsp::MWOCP6X_SECONDARY_FIRMWARE,
                ] {
                    if let Some(firmware) = firmware {
                        if revision.get(firmware.mcu) == firmware.revision {
                            // This MCU's firmware is already up to date
                            continue;
                        }
                        if self.status(firmware.mcu).is_backoff_elapsed(now) {
                            let _ = to_update.push(firmware);
                        } else {
                            ringbuf_entry!(Trace::BackingOff(
                                ndx,
                                firmware.mcu
                            ));
                        }
                    }
                }
                to_update
            }
        }
    }
}

fn update_firmware(
    mcu_status: &mut UpdateStatus,
    dev: &bsp::Mwocp6x,
    ndx: u8,
    firmware: FirmwareImage,
) {
    ringbuf_entry!(Trace::AttemptingUpdate(ndx, firmware.mcu));
    mcu_status.update_started = Some(Ticks::now());

    //
    // Before we start, update our backoff.  We'll double our backoff, up
    // to a cap of around a day.
    //
    mcu_status.update_backoff = match mcu_status.update_backoff {
        Some(backoff) if backoff.0 < 86_400_000 => Some(Ticks(backoff.0 * 2)),
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

        ringbuf_entry!(Trace::UpdateFailed(ndx, firmware.mcu));
        ringbuf_entry!(Trace::UpdateFailedState(state));
        mcu_status.update_failure = Some((Ticks::now(), state, err));
    };

    loop {
        match dev.update(state, firmware) {
            Err(err) => {
                update_failed(state, Some(err));
                break;
            }

            Ok((bsp::UpdateState::UpdateSuccessful, delay)) => {
                ringbuf_entry!(Trace::UpdateState(
                    bsp::UpdateState::UpdateSuccessful
                ));
                ringbuf_entry!(Trace::UpdateDelay(delay));
                hl::sleep_for(delay);

                let state = Some(bsp::UpdateState::UpdateSuccessful);

                //
                // We should be back up!  As a final measure, we are going
                // to check that the firmware revision matches the
                // revision we think we just wrote.  If it doesn't, there
                // is something amiss:  it may be that the image is
                // corrupt or that the version doesn't otherwise match.
                // Regardless, we consider that to be an update failure.
                //
                match dev.firmware_revision() {
                    Ok(revision)
                        if revision.get(firmware.mcu) != firmware.revision =>
                    {
                        update_failed(state, None);
                        break;
                    }

                    Err(err) => {
                        update_failed(state, Some(err));
                        break;
                    }

                    Ok(_) => {}
                }

                ringbuf_entry!(Trace::UpdateSucceeded(ndx, firmware.mcu));
                mcu_status.update_succeeded = Some(Ticks::now());
                mcu_status.update_backoff = None;
                break;
            }

            Ok((next, delay)) => {
                match next {
                    bsp::UpdateState::WroteBlock { .. } => {
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

#[unsafe(export_name = "main")]
fn main() -> ! {
    let i2c_task = I2C.get_task_id();

    let psus = PSU.claim();

    let devs: [bsp::Mwocp6x; PSU_COUNT] =
        array_init::array_init(|ndx: usize| {
            bsp::Mwocp6x::new(&bsp::DEVICES[ndx](i2c_task), 0)
        });

    loop {
        hl::sleep_for(TIMER_INTERVAL_MS);

        for (ndx, psu) in psus.iter_mut().enumerate() {
            let dev = &devs[ndx];
            let ndx = ndx as u8;

            let updates = psu.updates_to_attempt(dev, ndx);
            if updates.is_empty() {
                continue;
            }
            let was_power_good = match dev.power_good() {
                Ok(true) => {
                    ringbuf_entry!(Trace::PowerGoodBefore(ndx));
                    true
                }
                Ok(false) => {
                    ringbuf_entry!(Trace::PowerNotGoodBefore(ndx));
                    false
                }
                Err(error) => {
                    ringbuf_entry!(Trace::PGErrorBefore(ndx, error));
                    false
                }
            };

            for firmware in updates {
                update_firmware(psu.status(firmware.mcu), dev, ndx, firmware);
            }

            //
            // We're on the new firmware! And now, a final final check: make
            // sure that if we were power-good before the update, we are still
            // power-good now. It is very unclear what to do here if are no
            // longer power-good: it certainly seems possible that we have put a
            // firmware update on this PSU which has somehow incapacitated it.
            // We would rather not put the system in a compromised state by
            // continuing to potentially brick PSUs -- but we also want to
            // assure that we make progress should this ever resolve (e.g., by
            // pulling the bricked PSU). We will remain here until we see the
            // updated PSU go power-good; if it never does, we will at least not
            // attempt to put the (potentially) bad update anywhere else! If the
            // PSU was not power-good *before* the update, however, then it had
            // some sort of pre-existing condition that is not the fault of the
            // update, and we should continue applying the update to other PSUs.
            //
            loop {
                match dev.power_good() {
                    Ok(true) => {
                        ringbuf_entry!(Trace::PowerGoodAfter(ndx));
                        break;
                    }
                    Ok(false) => {
                        ringbuf_entry!(Trace::PowerNotGoodAfter(ndx));
                    }
                    Err(error) => {
                        ringbuf_entry!(Trace::PGErrorAfter(ndx, error));
                    }
                }
                if !was_power_good {
                    break;
                }
                hl::sleep_for(TIMER_INTERVAL_MS);
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
