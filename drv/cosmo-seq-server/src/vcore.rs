// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
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
use serde::Serialize;
use task_packrat_api::{self, Packrat};
use userlib::{sys_get_timer, units, TaskId};

pub(super) struct VCore {
    /// `PWR_CONT1`: This regulator controls `VDDCR_CPU0` and `VDDCR_SOC` rails.
    vddcr_cpu0: Raa229620A,
    /// `PWR_CONT2`: This regulator controls `VDDCR_CPU1` and `VDDIO_SP5` rails.
    vddcr_cpu1: Raa229620A,
    packrat: task_packrat_api::Packrat,
}

#[derive(Copy, Clone, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Rail {
    VddcrCpu0,
    VddcrCpu1,
}

#[derive(Copy, Clone, PartialEq)]
enum PmbusCmd {
    LoadLimit,
    ClearFaults,
    ReadVin,
    Status,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Initializing,
    Initialized,
    LimitsLoaded,
    FaultsCleared(Rails),
    PmbusAlert {
        timestamp: u64,
        rails: Rails,
    },
    Reading {
        timestamp: u64,
        vddcr_cpu0_vin: units::Volts,
        vddcr_cpu1_vin: units::Volts,
    },
    RegulatorStatus {
        rail: Rail,
        power_good: bool,
        faulted: bool,
    },
    StatusWord(Rail, Result<u16, ResponseCode>),
    StatusInput(Rail, Result<u8, ResponseCode>),
    StatusVout(Rail, Result<u8, ResponseCode>),
    StatusIout(Rail, Result<u8, ResponseCode>),
    StatusTemperature(Rail, Result<u8, ResponseCode>),
    StatusCml(Rail, Result<u8, ResponseCode>),
    StatusMfrSpecific(Rail, Result<u8, ResponseCode>),
    I2cError(Rail, PmbusCmd, raa229620a::Error),
    EreportSent(Rail, usize),
    EreportLost(Rail, usize, packrat::EreportWriteError),
    EreportTooBig(Rail),
}

#[derive(Copy, Clone, PartialEq)]
pub struct Rails {
    pub vddcr_cpu0: bool,
    pub vddcr_cpu1: bool,
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
///
/// (Read: I just took the number Bryan picked in the Gimlet sequencer,
/// divided it by 2, and copied his comment lol)
///
const VCORE_NSAMPLES: usize = 25;

impl VCore {
    pub fn new(i2c: TaskId, packrat: task_packrat_api::Packrat) -> Self {
        let (device, rail) = i2c_config::pmbus::vddcr_cpu0_a0(i2c);
        let vddcr_cpu0 = Raa229620A::new(&device, rail);

        let (device, rail) = i2c_config::pmbus::vddcr_cpu1_a0(i2c);
        let vddcr_cpu1 = Raa229620A::new(&device, rail);
        Self {
            vddcr_cpu0,
            vddcr_cpu1,
            packrat,
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
            retry_i2c_txn(Rail::VddcrCpu1, PmbusCmd::ClearFaults, || {
                self.vddcr_cpu1.clear_faults()
            })?;
        }

        ringbuf_entry!(Trace::FaultsCleared(which_rails));

        Ok(())
    }

    pub fn handle_pmbus_alert(&self, mut rails: Rails, now: u64) {
        ringbuf_entry!(Trace::PmbusAlert {
            timestamp: now,
            rails,
        });

        let cpu0_state =
            self.record_pmbus_status(now, Rail::VddcrCpu0, rails.vddcr_cpu0);
        rails.vddcr_cpu0 |= cpu0_state.faulted;

        let cpu1_state =
            self.record_pmbus_status(now, Rail::VddcrCpu1, rails.vddcr_cpu1);
        rails.vddcr_cpu1 |= cpu1_state.faulted;

        if cpu0_state.input_fault || cpu1_state.input_fault {
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
                        // We don't retry I2C errors here, since we're just going
                        // to take another reading anyway.
                        ringbuf_entry!(Trace::I2cError(
                            Rail::VddcrCpu1,
                            PmbusCmd::ReadVin,
                            e
                        ));
                        units::Volts(f32::NAN)
                    });
                //
                // Record our readings, along with a timestamp. On the one hand,
                // it's a little exceesive to record a timestamp on every
                // reading: it's in milliseconds, and because it takes ~900µs
                // per reading (so ~1800us to read from both regulators), we
                // expect the timestamp to (basically) be incremented by 2 with
                // every reading (with a duplicate timestamp occuring every ~7-9
                // entries). But on the other, it's not impossible to be
                // preempted, and it's valuable to have as tight a coupling as
                // possible between observed reading and observed time.
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

        // The only way to make the pins deassert (and thus, the IRQ go
        // away) is to tell the guys to clear the fault.
        // N.B.: unlike other FPGA sequencer alerts, we need not clear the
        // IFR bits for these; they are hot as long as the PMALERT pin from
        // the RAA229620As is asserted.
        //
        // Per the RAA229620A datasheet (R16DS0309EU0200 Rev.2.00, page 36),
        // clearing the fault in the regulator will deassert PMALERT_L,
        // releasing the IRQ, but the fault bits to be reset if the fault
        // condition still exists. Note that this does *not* cause the device to
        // restart if it has shut down. The behavior of "CLEAR_FAULTS" is really
        // much closer to "ACKNOWLEDGE_PMBUS_ALERT", since it doesn't actually
        // seem to effect the state of the regulator.
        //
        // TODO(eliza): we will want to handle a shut down regulator more
        // intelligently in future...
        let _ = self.clear_faults(rails);
    }

    fn record_pmbus_status(
        &self,
        now: u64,
        rail: Rail,
        alerted: bool,
    ) -> RegulatorState {
        use pmbus::commands::raa229620a::STATUS_WORD;

        let device = match rail {
            Rail::VddcrCpu0 => &self.vddcr_cpu0,
            Rail::VddcrCpu1 => &self.vddcr_cpu1,
        };

        // Read the status word, and figure out what's going on with this VRM.
        let status_word =
            retry_i2c_txn(rail, PmbusCmd::Status, || device.status_word());

        ringbuf_entry!(Trace::StatusWord(rail, status_word.map(|s| s.0)));
        let mut faulted = alerted;
        let mut input_fault = false;
        let power_good = if let Ok(status) = status_word {
            // If any fault bits are hot, set this VRM to "faulted", even if it
            // was not the one whose `PMALERT` assertion actually triggered our
            // IRQ.
            //
            // Note: since these are all single bits in the PMBus STATUS_WORD,
            // the PMBus crate *should* never return `None` for them, as there
            // are no un-interpretable values possible. Either a bit is set or
            // it is not.
            if status.get_input_fault()
                != Some(STATUS_WORD::InputFault::NoFault)
            {
                faulted = true;
                // If the INPUT_FAULT bit is set, we will also sample input
                // voltage readings into the ringbuf.
                input_fault = true;
            }
            faulted |= status.get_output_voltage_fault()
                != Some(STATUS_WORD::OutputVoltageFault::NoFault);
            faulted |= status.get_output_voltage_fault()
                != Some(STATUS_WORD::OutputVoltageFault::NoFault);
            faulted |= status.get_other_fault()
                != Some(STATUS_WORD::OtherFault::NoFault);
            faulted |= status.get_manufacturer_fault()
                != Some(STATUS_WORD::ManufacturerFault::NoFault);
            faulted |=
                status.get_cml_fault() != Some(STATUS_WORD::CMLFault::NoFault);
            faulted |= status.get_temperature_fault()
                != Some(STATUS_WORD::TemperatureFault::NoFault);

            // If the POWER_GOOD# bit is set, the regulator has deasserted its
            // POWER_GOOD pin.
            //
            // Again, this *shouldn't* ever be `None`, as it's a single bit.
            let power_good = status.get_power_good_status()
                == Some(STATUS_WORD::PowerGoodStatus::PowerGood);

            ringbuf_entry!(Trace::RegulatorStatus {
                rail,
                power_good,
                faulted,
            });
            Some(power_good)
        } else {
            None
        };

        // If we haven't faulted, and POWER_GOOD is asserted, nothing left
        // to do here.
        if !faulted && power_good == Some(true) {
            return RegulatorState {
                faulted,
                input_fault,
            };
        }

        // Read PMBus status registers and prepare an ereport.
        let status_input =
            retry_i2c_txn(rail, PmbusCmd::Status, || device.status_input())
                .map(|s| s.0);
        ringbuf_entry!(Trace::StatusInput(rail, status_input));

        // If any bits are set in the STATUS_INPUT register, then this is an
        // input fault, so we should perform sampling of the input voltage for
        // the ringbuf.
        if status_input != Ok(0) {
            input_fault = true;
        }

        let status_vout =
            retry_i2c_txn(rail, PmbusCmd::Status, || device.status_vout())
                .map(|s| s.0);
        ringbuf_entry!(Trace::StatusVout(rail, status_vout));
        let status_iout =
            retry_i2c_txn(rail, PmbusCmd::Status, || device.status_iout())
                .map(|s| s.0);
        ringbuf_entry!(Trace::StatusIout(rail, status_iout));
        let status_temperature = retry_i2c_txn(rail, PmbusCmd::Status, || {
            device.status_temperature()
        })
        .map(|s| s.0);
        ringbuf_entry!(Trace::StatusTemperature(rail, status_temperature));
        let status_cml =
            retry_i2c_txn(rail, PmbusCmd::Status, || device.status_cml())
                .map(|s| s.0);
        ringbuf_entry!(Trace::StatusCml(rail, status_cml));
        let status_mfr = retry_i2c_txn(rail, PmbusCmd::Status, || {
            device.status_mfr_specific()
        })
        .map(|s| s.0);
        ringbuf_entry!(Trace::StatusMfrSpecific(rail, status_mfr));

        let status = PmbusStatus {
            word: status_word.map(|s| s.0).ok(),
            input: status_input.ok(),
            vout: status_vout.ok(),
            iout: status_iout.ok(),
            temp: status_temperature.ok(),
            cml: status_cml.ok(),
            mfr: status_mfr.ok(),
        };

        let ereport = Ereport {
            k: "pmbus.alert",
            v: 0,
            rail,
            dev_id: device.i2c_device().component_id(),
            time: now,
            status,
            pwr_good: power_good,
        };
        deliver_ereport(rail, &self.packrat, &ereport);

        RegulatorState {
            faulted,
            input_fault,
        }
    }
}

#[derive(Serialize)]
struct Ereport {
    k: &'static str,
    v: usize,
    dev_id: &'static str,
    rail: Rail,
    time: u64,
    pwr_good: Option<bool>,
    status: PmbusStatus,
}

#[derive(Copy, Clone, Default, Serialize)]
struct PmbusStatus {
    word: Option<u16>,
    input: Option<u8>,
    iout: Option<u8>,
    vout: Option<u8>,
    temp: Option<u8>,
    cml: Option<u8>,
    mfr: Option<u8>,
}

struct RegulatorState {
    faulted: bool,
    input_fault: bool,
}

// Mostly stolen from the same thing in gimlet-seq, but with an added `Rail` to
// indicate which device we're talking to.
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

// This is in its own function so that the ereport buffer is only on the stack
// while we're using it, and not for the entireity of `record_pmbus_status`,
// which calls into a bunch of other functions. This may reduce our stack depth
// a bit.
#[inline(never)]
fn deliver_ereport(
    rail: Rail,
    packrat: &Packrat,
    data: &impl serde::Serialize,
) {
    let mut ereport_buf = [0u8; 256];
    let writer = minicbor::encode::write::Cursor::new(&mut ereport_buf[..]);
    let mut s = minicbor_serde::Serializer::new(writer);
    match data.serialize(&mut s) {
        Ok(_) => {
            let len = s.into_encoder().into_writer().position();
            match packrat.deliver_ereport(&ereport_buf[..len]) {
                Ok(_) => {
                    ringbuf_entry!(Trace::EreportSent(rail, len));
                }
                Err(e) => {
                    ringbuf_entry!(Trace::EreportLost(rail, len, e));
                }
            }
            ringbuf_entry!(Trace::EreportSentOff(rail, len));
        }
        Err(_) => {
            // XXX(eliza): ereport didn't fit in buffer...what do
            ringbuf_entry!(Trace::EreportTooBig(rail));
        }
    }
}
