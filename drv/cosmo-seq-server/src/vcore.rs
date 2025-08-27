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
use task_packrat_api::{self, EreportClass, Packrat};
use userlib::{sys_get_timer, units, TaskId};

pub(super) struct VCore {
    /// `PWR_CONT1`: This regulator controls `VDDCR_CPU0` and `VDDCR_SOC` rails.
    vddcr_cpu0: Raa229620A,
    /// `PWR_CONT2`: This regulator controls `VDDCR_CPU1` and `VDDIO_SP5` rails.
    vddcr_cpu1: Raa229620A,
    packrat: task_packrat_api::Packrat,
}

#[derive(Copy, Clone, PartialEq, serde::Serialize)]
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
    StatusWord,
    // StatusIout,
    // StatusVout,
    StatusInput,
    // StatusTemperature,
    // StatusCml,
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
    Fault {
        rail: Rail,
        status_word: u16,
    },
    VinFault {
        rail: Rail,
        status_input: Result<u8, ResponseCode>,
    },
    Reading {
        timestamp: u64,
        vddcr_cpu0_vin: units::Volts,
        vddcr_cpu1_vin: units::Volts,
    },
    I2cError(Rail, PmbusCmd, raa229620a::Error),
    VinSummary(Rail, VoltageRange),
    EreportSent(usize),
    EreportTooBig,
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
///(Regulator),
/// (Read: I just took the number Bryan picked in the Gimlet sequencer,
/// divided it by 2, and copied his comment lol)
///
const VCORE_NSAMPLES: usize = 25;

//
// Class string segments for ereports
//
static CLASS_VCORE: &str = "vcore";
static CLASS_PMALERT: &str = "pmbus_alert";
static CLASS_VIN: &str = "vin";
static CLASS_OVERCURRENT: &str = "overcurrent";
static CLASS_OVERVOLT: &str = "overvolt";
static CLASS_UNDERVOLT: &str = "undervolt";
static CLASS_OVERPOWER: &str = "overpower";
static CLASS_OTHER: &str = "other";
static CLASS_WARN: &str = "warn";
static CLASS_FAULT: &str = "fault";

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
            retry_i2c_txn(Rail::VddcrCpu0, PmbusCmd::ClearFaults, || {
                self.vddcr_cpu1.clear_faults()
            })?;
        }

        ringbuf_entry!(Trace::FaultsCleared(which_rails));

        Ok(())
    }

    pub fn handle_pmalert(&self, rails: Rails, now: u64) {
        //
        // We want to record min/max voltages on *both* rails as close to the
        // fault as possible, rather than spending 45ms recording one regulator,
        // and then (if both have faulted) moving on to do the next one. And, we
        // want to use the peaks from that initial period of sampling for both
        // ereports. So, stash them in an `Option` so that if we record ereports
        // for both rails, we'll populate that option for the first one.
        //
        let mut vin_ranges = None;
        // Similarly, we use the same timestamp for both ereports, since it's
        // the time our IRQ line was pulled. That way, we accurately report when
        // the IRQ happened, regardless of how long it takes us to actually
        // record data etc.

        ringbuf_entry!(Trace::Pmalert {
            timestamp: now,
            faulted: rails
        });

        if rails.vddcr_cpu0 {
            self.record_pmalert_on_rail(now, Rail::VddcrCpu0, &mut vin_ranges);
        }

        if rails.vddcr_cpu1 {
            self.record_pmalert_on_rail(now, Rail::VddcrCpu1, &mut vin_ranges);
        }

        // The only way to make the pins deassert (and thus, the IRQ go
        // away) is to tell the guys to clear the fault.
        // N.B.: unlike other FPGA sequencer alerts, we need not clear the
        // IFR bits for these; they are hot as long as the PMALERT pin from
        // the RAA229620As is asserted. Clearing the fault in the regulator
        // clears the IRQ.
        let _ = self.clear_faults(rails);
    }

    fn record_pmalert_on_rail(
        &self,
        now: u64,
        rail: Rail,
        vin_ranges: &mut Option<VoltageRanges>,
    ) {
        use pmbus::commands::raa229620a::STATUS_INPUT;
        use pmbus::commands::raa229620a::STATUS_WORD::InputFault;
        let device = match rail {
            Rail::VddcrCpu0 => &self.vddcr_cpu0,
            Rail::VddcrCpu1 => &self.vddcr_cpu1,
        };
        let Ok(status_word) =
            retry_i2c_txn(rail, PmbusCmd::StatusWord, || device.status_word())
        else {
            return;
        };
        ringbuf_entry!(Trace::Fault {
            rail,
            status_word: status_word.0
        });

        // TODO(eliza): we ought to handle other faults eventually...
        if status_word.get_input_fault() != Some(InputFault::NoFault) {
            let status_input =
                retry_i2c_txn(rail, PmbusCmd::StatusInput, || {
                    device.status_input()
                });
            ringbuf_entry!(Trace::VinFault {
                rail,
                status_input: status_input
                    .map(|STATUS_INPUT::CommandData(byte)| byte)
            });
            let class = match status_input {
                Ok(s)
                    if s.get_input_overcurrent_fault()
                        == Some(STATUS_INPUT::InputOvercurrentFault::Fault) =>
                {
                    // vcore.pmbus_alert.vin.overcurrent.fault
                    EreportClass(&[
                        CLASS_VCORE,
                        CLASS_PMALERT,
                        CLASS_VIN,
                        CLASS_OVERCURRENT,
                        CLASS_FAULT,
                    ])
                }
                Ok(s)
                    if s.get_input_overcurrent_warning()
                        == Some(
                            STATUS_INPUT::InputOvercurrentWarning::Warning,
                        ) =>
                {
                    // vcore.pmbus_alert.vin.overcurrent.warn
                    EreportClass(&[
                        CLASS_VCORE,
                        CLASS_PMALERT,
                        CLASS_VIN,
                        CLASS_OVERCURRENT,
                        CLASS_WARN,
                    ])
                }
                Ok(s)
                    if s.get_input_overvoltage_fault()
                        == Some(STATUS_INPUT::InputOvervoltageFault::Fault) =>
                {
                    // vcore.pmbus_alert.vin.overcurrent.fault
                    EreportClass(&[
                        CLASS_VCORE,
                        CLASS_PMALERT,
                        CLASS_VIN,
                        CLASS_OVERVOLT,
                        CLASS_FAULT,
                    ])
                }
                Ok(s)
                    if s.get_input_overvoltage_warning()
                        == Some(
                            STATUS_INPUT::InputOvervoltageWarning::Warning,
                        ) =>
                {
                    // vcore.pmbus_alert.vin.overcurrent.warn
                    EreportClass(&[
                        CLASS_VCORE,
                        CLASS_PMALERT,
                        CLASS_VIN,
                        CLASS_OVERVOLT,
                        CLASS_WARN,
                    ])
                }
                Ok(s)
                    if s.get_input_undervoltage_fault()
                        == Some(
                            STATUS_INPUT::InputUndervoltageFault::Fault,
                        ) =>
                {
                    // vcore.pmbus_alert.vin.undervolt.fault
                    EreportClass(&[
                        CLASS_VCORE,
                        CLASS_PMALERT,
                        CLASS_VIN,
                        CLASS_UNDERVOLT,
                        CLASS_FAULT,
                    ])
                }
                Ok(s)
                    if s.get_input_undervoltage_warning()
                        == Some(
                            STATUS_INPUT::InputUndervoltageWarning::Warning,
                        ) =>
                {
                    // vcore.pmbus_alert.vin.undervolt.warn
                    EreportClass(&[
                        CLASS_VCORE,
                        CLASS_PMALERT,
                        CLASS_VIN,
                        CLASS_UNDERVOLT,
                        CLASS_WARN,
                    ])
                }
                Ok(s)
                    if s.get_input_overpower_warning()
                        == Some(
                            STATUS_INPUT::InputOverpowerWarning::Warning,
                        ) =>
                {
                    // vcore.pmbus_alert.vin.undervolt.warn
                    EreportClass(&[
                        CLASS_VCORE,
                        CLASS_PMALERT,
                        CLASS_VIN,
                        CLASS_OVERPOWER,
                        CLASS_WARN,
                    ])
                }
                _ =>
                // vcore.pmbus_alert.vin.other
                {
                    EreportClass(&[
                        CLASS_VCORE,
                        CLASS_PMALERT,
                        CLASS_VIN,
                        CLASS_OTHER,
                    ])
                }
            };

            // TODO(eliza): if we saw an IRQ from one VRM, and the other one
            // also dips while we're sampling,c an we make an ereport for it
            // too? Figure that out...
            let ranges = vin_ranges.get_or_insert_with(|| self.record_vin());
            let vin = match rail {
                Rail::VddcrCpu0 => ranges.vddcr_cpu0,
                Rail::VddcrCpu1 => ranges.vddcr_cpu1,
            };

            // "Houston, we've got a main bus B undervolt..."
            let ereport = VinEreport {
                rail,
                vin,
                time: now,
                dev_id: device.i2c_device().component_id(),
                status: PmbusStatus {
                    word: status_word.0,
                    input: status_input
                        .map(|STATUS_INPUT::CommandData(byte)| byte)
                        .ok(),
                    ..Default::default()
                },
            };
            deliver_ereport(&self.packrat, &class, &ereport);
        }
    }
    fn record_vin(&self) -> VoltageRanges {
        #[derive(Default)]
        struct Stats {
            min: f32,
            max: f32,
            sum: f32,
            ngood: u32,
        }
        impl Stats {
            fn record(&mut self, units::Volts(vin): units::Volts) {
                self.min = f32::min(self.min, vin);
                self.max = f32::max(self.max, vin);
                self.sum += vin;
                self.ngood += 1;
            }

            fn range(&self) -> VoltageRange {
                VoltageRange {
                    min: self.min,
                    max: self.max,
                    avg: self.sum / self.ngood as f32,
                }
            }
        }
        let mut cpu0_stats = Stats::default();
        let mut cpu1_stats = Stats::default();

        for _ in 0..VCORE_NSAMPLES {
            let vddcr_cpu0_vin = match self.vddcr_cpu0.read_vin() {
                Ok(vin) => {
                    cpu0_stats.record(vin);
                    vin
                }
                Err(e) => {
                    // We don't retry I2C errors here, since we're just going
                    // to take another reading anyway.
                    ringbuf_entry!(Trace::I2cError(
                        Rail::VddcrCpu0,
                        PmbusCmd::ReadVin,
                        e
                    ));
                    units::Volts(f32::NAN)
                }
            };

            let vddcr_cpu1_vin = match self.vddcr_cpu1.read_vin() {
                Ok(vin) => {
                    cpu1_stats.record(vin);
                    vin
                }
                Err(e) => {
                    // We don't retry I2C errors here, since we're just going
                    // to take another reading anyway.
                    ringbuf_entry!(Trace::I2cError(
                        Rail::VddcrCpu1,
                        PmbusCmd::ReadVin,
                        e
                    ));
                    units::Volts(f32::NAN)
                }
            };

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

        let vddcr_cpu0 = cpu0_stats.range();
        let vddcr_cpu1 = cpu1_stats.range();
        ringbuf_entry!(Trace::VinSummary(Rail::VddcrCpu0, vddcr_cpu0));
        ringbuf_entry!(Trace::VinSummary(Rail::VddcrCpu1, vddcr_cpu1));

        VoltageRanges {
            vddcr_cpu0,
            vddcr_cpu1,
        }
    }
}

#[derive(serde::Serialize)]
struct VinEreport {
    rail: Rail,
    vin: VoltageRange,
    time: u64,
    status: PmbusStatus,
    dev_id: &'static str,
}

struct VoltageRanges {
    vddcr_cpu0: VoltageRange,
    vddcr_cpu1: VoltageRange,
}

#[derive(Copy, Clone, PartialEq, Default, serde::Serialize)]
struct VoltageRange {
    min: f32,
    max: f32,
    avg: f32,
}

#[derive(Copy, Clone, Default, serde::Serialize)]
struct PmbusStatus {
    word: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    iout: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vout: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cml: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmp: Option<u8>,
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

// This is in its own function so that we only push a stack frame large enough
// for the ereport buffer if needed.
#[inline(never)]
fn deliver_ereport(
    packrat: &Packrat,
    class: &EreportClass<'_>,
    data: &impl serde::Serialize,
) {
    let mut ereport_buf = [0u8; 256];
    let report = task_packrat_api::SerdeEreport { class, data };
    let writer = minicbor::encode::write::Cursor::new(&mut ereport_buf[..]);
    match report.to_writer(writer) {
        Ok(writer) => {
            let len = writer.position();
            packrat.deliver_ereport(&ereport_buf[..len]);
            ringbuf_entry!(Trace::EreportSent(len));
        }
        Err(_) => {
            // XXX(eliza): ereport didn't fit in buffer...what do
            ringbuf_entry!(Trace::EreportTooBig)
        }
    }
}
