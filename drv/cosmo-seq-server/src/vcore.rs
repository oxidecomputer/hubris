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
use drv_i2c_devices::raa229620a::Raa229620A;
use ringbuf::*;
use userlib::{sys_get_timer, units, TaskId};

pub(super) struct VCore {
    /// `PWR_CONT1`: This regulator controls `VDDCR_CPU0` and `VDDCR_SOC` rails.
    vddcr_cpu0: Raa229620A,
    /// `PWR_CONT2`: This regulator controls `VDDCR_CPU1` and `VDDIO_SP5` rails.
    vddcr_cpu1: Raa229620A,
}

#[derive(Copy, Clone, PartialEq)]
enum Regulator {
    VddcrCpu0,
    VddcrCpu1,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Initializing,
    Initialized,
    LimitLoaded(Regulator),
    FaultsCleared(Regulator),
    Fault(WhichRails),
    Reading {
        timestamp: u64,
        vddcr_cpu0_vin: units::Volts,
        vddcr_cpu1_vin: units::Volts,
    },
    Error(Regulator, ResponseCode),
}

#[derive(Copy, Clone, PartialEq)]
pub struct WhichRails {
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
        self.vddcr_cpu0.set_vin_uv_warn_limit(VCORE_UV_WARN_LIMIT)?;
        ringbuf_entry!(Trace::LimitLoaded(Regulator::VddcrCpu0));
        self.vddcr_cpu1.set_vin_uv_warn_limit(VCORE_UV_WARN_LIMIT)?;
        ringbuf_entry!(Trace::LimitLoaded(Regulator::VddcrCpu1));

        // Clear our faults
        self.clear_faults(WhichRails {
            vddcr_cpu0: true,
            vddcr_cpu1: true,
        })?;

        // The higher-level sequencer code will unmask the FPGA interrupts for
        // our guys.

        ringbuf_entry!(Trace::Initialized);

        Ok(())
    }

    pub fn clear_faults(
        &self,
        WhichRails {
            vddcr_cpu0,
            vddcr_cpu1,
        }: WhichRails,
    ) -> Result<(), ResponseCode> {
        if vddcr_cpu0 {
            self.vddcr_cpu0.clear_faults()?;
            ringbuf_entry!(Trace::FaultsCleared(Regulator::VddcrCpu0));
        }

        if vddcr_cpu1 {
            self.vddcr_cpu1.clear_faults()?;
            ringbuf_entry!(Trace::FaultsCleared(Regulator::VddcrCpu1));
        }

        Ok(())
    }

    pub fn record_undervolt(&self, which_rails: WhichRails) {
        ringbuf_entry!(Trace::Fault(which_rails));

        for _ in 0..VCORE_NSAMPLES {
            let vddcr_cpu0_vin =
                self.vddcr_cpu0.read_vin().unwrap_or_else(|code| {
                    ringbuf_entry!(Trace::Error(
                        Regulator::VddcrCpu0,
                        code.into()
                    ));
                    units::Volts(f32::NAN)
                });

            let vddcr_cpu1_vin =
                self.vddcr_cpu1.read_vin().unwrap_or_else(|code| {
                    ringbuf_entry!(Trace::Error(
                        Regulator::VddcrCpu1,
                        code.into()
                    ));
                    units::Volts(f32::NAN)
                });

            //
            // Record our reading, along with a timestamp.  On the
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
