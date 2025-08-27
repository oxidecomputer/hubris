// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//

//!
//! Cosmo V_core_ monitoring.
//!
//! This is basically the same as the similarly named module in the Gimlet
//! sequencer, but we have two RAA22960A regulators driving the `VDDCR_CPU0` and
//! `VDDCR_CPU1` rails, rather than one RAA229618. Also unlike Gimlet, the PMBus
//! `PMALERT_L` pins from the power controller go to the FPGA, so rather than
//! watching those pins directly via EXTI, we handle the FPGA to sequencer
//! interrupt and call into this module should the PMBus alerts for these
//! regulators be asserted.
//!

use super::i2c_config;
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::raa229620a::{self, Raa229620A};
use ringbuf::*;
use userlib::{sys_get_timer, units, TaskId};

pub(super) struct VCore {
    /// `PWR_CONT1`: This regulator controls `VDDCR_CPU0` and `VDDCR_SOC` rails.
    vddcr_cpu0: Raa229620A,
    /// `PWR_CONT2`: This regulator controls `VDDCR_CPU1` and `VDDIO_SP5` rails.
    vddcr_cpu1: Raa229620A,
}

#[derive(Copy, Clone, PartialEq)]
enum Rail {
    VddcrCpu0,
    VddcrCpu1,
}

#[derive(Copy, Clone, PartialEq)]
enum PmbusCmd {
    LoadLimit,
    ClearFaults,
    ReadVin,
    StatusWord,
    StatusIout,
    StatusVout,
    StatusInput,
    StatusTemperature,
    StatusCml,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Initializing,
    Initialized,
    LimitsLoaded,
    FaultsCleared(Rails),
    Pmalert {
        timestamp: u64,
        faulted: Rails,
    },
    VinFault(Rail),
    Status(Rail, PmbusStatus),
    Reading {
        timestamp: u64,
        vddcr_cpu0_vin: units::Volts,
        vddcr_cpu1_vin: units::Volts,
    },
    I2cError(Rail, PmbusCmd, raa229620a::Error),
}

#[derive(Copy, Clone, PartialEq)]
pub struct Rails {
    pub vddcr_cpu0: bool,
    pub vddcr_cpu1: bool,
}

#[derive(Copy, Clone, PartialEq)]
pub struct PmbusStatus {
    status_word: u16,
    status_iout: u8,
    status_vout: u8,
    status_input: u8,
    status_temperature: u8,
    status_cml: u8,
}

ringbuf!(Trace, 60, Trace::None);

///
/// We are going to set our input undervoltage warn limit to be 11.75 volts.
/// Note that we will not fault if VIN goes below this (that is, we will not
/// lose POWER_GOOD), but the part will indicate an input fault and pull
/// on its PMBus alert pin.
///
const VCORE_UV_WARN_LIMIT: units::Volts = units::Volts(11.75);

///
/// We want to collect enough samples (at ~900µs per sample per regulator, or
/// ~1.8ms for both) to adequately cover any anticipated dip.  We have seen
/// these have an ~11ms total width in the wild, so we give ourselves plenty
/// of margin here and get ~45ms of data.
///(Regulator),
/// (Read: I just took the number Bryan picked in the Gimlet sequencer,
/// divided it by 2, and copied his comment lol)
///
const VCORE_NSAMPLES: usize = 25;

impl VCore {
    pub fn new(i2c: TaskId) -> Self {
        let (device, rail) = i2c_config::pmbus::vddcr_cpu0_a0(i2c);
        let vddcr_cpu0 = Raa229620A::new(&device, rail);

        let (device, rail) = i2c_config::pmbus::vddcr_cpu1_a0(i2c);
        let vddcr_cpu1 = Raa229620A::new(&device, rail);
        Self {
            vddcr_cpu0,
            vddcr_cpu1,
        }
    }

    pub fn initialize_uv_warning(&self) -> Result<(), ResponseCode> {
        ringbuf_entry!(Trace::Initializing);

        // Set our warn limit
        retry_i2c_txn(Rail::VddcrCpu0, PmbusCmd::LoadLimit, || {
            self.vddcr_cpu0.set_vin_uv_warn_limit(VCORE_UV_WARN_LIMIT)
        })?;
        retry_i2c_txn(Rail::VddcrCpu1, PmbusCmd::LoadLimit, || {
            self.vddcr_cpu1.set_vin_uv_warn_limit(VCORE_UV_WARN_LIMIT)
        })?;
        ringbuf_entry!(Trace::LimitsLoaded);

        // Clear our faults
        self.clear_faults(Rails {
            vddcr_cpu0: true,
            vddcr_cpu1: true,
        })?;

        // The higher-level sequencer code will unmask the FPGA interrupts for
        // our guys.

        ringbuf_entry!(Trace::Initialized);

        Ok(())
    }

    pub fn clear_faults(&self, which_rails: Rails) -> Result<(), ResponseCode> {
        if which_rails.vddcr_cpu0 {
            retry_i2c_txn(Rail::VddcrCpu0, PmbusCmd::ClearFaults, || {
                self.vddcr_cpu0.clear_faults()
            })?;
        }

        if which_rails.vddcr_cpu1 {
            retry_i2c_txn(Rail::VddcrCpu0, PmbusCmd::ClearFaults, || {
                self.vddcr_cpu1.clear_faults()
            })?;
        }

        ringbuf_entry!(Trace::FaultsCleared(which_rails));

        Ok(())
    }

    pub fn handle_pmalert(&self, rails: Rails) {
        ringbuf_entry!(Trace::Pmalert {
            timestamp: sys_get_timer().now,
            faulted: rails
        });

        let mut is_vin = false;

        fn read_pmalert_status(
            device: &Raa229620A,
            rail: Rail,
            is_vin: &mut bool,
        ) -> Result<PmbusStatus, ()> {
            use pmbus::commands::raa229620a::STATUS_WORD as status_word;
            let status_word = retry_i2c_txn(rail, PmbusCmd::StatusWord, || {
                device.status_word()
            })
            .map_err(|_| ())?;

            if status_word.get_input_fault()
                != Some(status_word::InputFault::NoFault)
            {
                ringbuf_entry!(Trace::VinFault(rail));
                *is_vin = true;
            }
            let status_vout = retry_i2c_txn(rail, PmbusCmd::StatusVout, || {
                device.status_vout()
            })
            .map_err(|_| ())?;
            let status_iout = retry_i2c_txn(rail, PmbusCmd::StatusIout, || {
                device.status_iout()
            })
            .map_err(|_| ())?;
            let status_input =
                retry_i2c_txn(rail, PmbusCmd::StatusInput, || {
                    device.status_input()
                })
                .map_err(|_| ())?;
            let status_temperature =
                retry_i2c_txn(rail, PmbusCmd::StatusTemperature, || {
                    device.status_temperature()
                })
                .map_err(|_| ())?;
            let status_cml = retry_i2c_txn(rail, PmbusCmd::StatusCml, || {
                device.status_cml()
            })
            .map_err(|_| ())?;
            Ok(PmbusStatus {
                status_word: status_word.0,
                status_iout: status_iout.0,
                status_vout: status_vout.0,
                status_input: status_input.0,
                status_temperature: status_temperature.0,
                status_cml: status_cml.0,
            })
        }

        if rails.vddcr_cpu0 {
            if let Ok(status) = read_pmalert_status(
                &self.vddcr_cpu0,
                Rail::VddcrCpu0,
                &mut is_vin,
            ) {
                ringbuf_entry!(Trace::Status(Rail::VddcrCpu0, status));
            }
        }

        if rails.vddcr_cpu1 {
            if let Ok(status) = read_pmalert_status(
                &self.vddcr_cpu1,
                Rail::VddcrCpu1,
                &mut is_vin,
            ) {
                ringbuf_entry!(Trace::Status(Rail::VddcrCpu1, status));
            }
        }

        if is_vin {
            self.record_vin();
        }
    }

    fn record_vin(&self) {
        for _ in 0..VCORE_NSAMPLES {
            let vddcr_cpu0_vin =
                self.vddcr_cpu0.read_vin().unwrap_or_else(|e| {
                    // We don't retry I2C errors here, since we're just going
                    // to take another reading anyway.
                    ringbuf_entry!(Trace::I2cError(
                        Rail::VddcrCpu0,
                        PmbusCmd::ReadVin,
                        e
                    ));
                    units::Volts(f32::NAN)
                });

            let vddcr_cpu1_vin =
                self.vddcr_cpu1.read_vin().unwrap_or_else(|e| {
                    ringbuf_entry!(Trace::I2cError(
                        Rail::VddcrCpu1,
                        PmbusCmd::ReadVin,
                        e
                    ));
                    units::Volts(f32::NAN)
                });

            //
            // Record our readings, along with a timestamp.  On the
            // one hand, it's a little exceesive to record a
            // timestamp on every reading:  it's in milliseconds,
            // and because it takes ~900µs per reading, we expect
            // the timestamp to (basically) be incremented by 2 with
            // every reading (with a duplicate timestamp occuring
            // every ~7-9 entries).  But on the other, it's not
            // impossible to be preempted, and it's valuable to have
            // as tight a coupling as possible between observed
            // reading and observed time.
            //
            // HI BRYAN I COPIED UR HOMEWORK AGAIN :) :) :)
            //
            ringbuf_entry!(Trace::Reading {
                timestamp: sys_get_timer().now,
                vddcr_cpu0_vin,
                vddcr_cpu1_vin,
            });
        }
    }
}

// Mostly stolen from the same thing in gimlet-seq.
fn retry_i2c_txn<T>(
    rail: Rail,
    which: PmbusCmd,
    mut txn: impl FnMut() -> Result<T, raa229620a::Error>,
) -> Result<T, ResponseCode> {
    // Chosen by fair dice roll, seems reasonable-ish?
    let mut retries_remaining = 3;
    loop {
        match txn() {
            Ok(x) => return Ok(x),
            Err(e) => {
                ringbuf_entry!(Trace::I2cError(rail, which, e));

                if retries_remaining == 0 {
                    return Err(e.into());
                }

                retries_remaining -= 1;
            }
        }
    }
}
