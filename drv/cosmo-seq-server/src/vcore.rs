// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//

///
/// Cosmo V_core_ monitoring.
///
/// This is basically the same as the similarly named module in the Gimlet
/// sequencer, but we have two RAA22960A regulators driving the `VDDCR_CPU0` and
/// `VDDCR_CPU1` rails, rather than one RAA229618. Also unlike Gimlet, the PMBus
/// `PMALERT_L` pins from the power controller go to the FPGA, so rather than
/// watching those pins directly via EXTI, we handle the FPGA to sequencer
/// interrupt and call into this module should the PMBus alerts for these
/// regulators be asserted.
///
use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_i2c_devices::raa229620::Raa229620;
use drv_stm32xx_sys_api as sys_api;
use ringbuf::*;
use sys_api::IrqControl;
use userlib::{sys_get_timer, units};

pub(super) struct VCore {
    /// This regulator controls `VDDCR_CPU0` and `VDDCR_SOC` rails.
    pwr_cont1: Raa229620,
    /// This regulator controls `VDDCR_CPU1` and `VDDIO_SP5` rails.
    pwr_cont2: Raa229620,
}

#[derive(Copy, Clone, PartialEq)]
enum Regulator {
    PwrCont1,
    PwrCont2,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Initializing,
    Initialized,
    LimitLoaded(Regulator),
    FaultsCleared(Regulator),
    Reading {
        timestamp: u64,
        pwr_cont1_vin: units::Volts,
        pwr_cont2_vin: units::Volts,
    },
    Error(Regulator, ResponseCode),
}

ringbuf!(Trace, 120, Trace::None);

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
    pub fn initialize_uv_warning(&self) -> Result<(), ResponseCode> {
        ringbuf_entry!(Trace::Initializing);

        // Set our warn limit
        self.pwr_cont1.set_vin_uv_warn_limit(VCORE_UV_WARN_LIMIT)?;
        ringbuf_entry!(Trace::LimitLoaded(Regulator::PwrCont1));
        self.pwr_cont2.set_vin_uv_warn_limit(VCORE_UV_WARN_LIMIT)?;
        ringbuf_entry!(Trace::LimitLoaded(Regulator::PwrCont2));

        // Clear our faults
        self.clear_faults()?;

        // The higher-level sequencer code will unmask the FPGA interrupts for
        // our guys.

        ringbuf_entry!(Trace::Initialized);

        Ok(())
    }

    pub fn clear_faults(&self) -> Result<(), ResponseCode> {
        self.pwr_cont1.clear_faults()?;
        ringbuf_entry!(Trace::FaultsCleared(Regulator::PwrCont1));
        self.pwr_cont2.clear_faults()?;
        ringbuf_entry!(Trace::FaultsCleared(Regulator::PwrCont2));

        Ok(())
    }

    pub fn record_undervolt(&self) {
        ringbuf_entry!(Trace::Fault);

        for _ in 0..VCORE_NSAMPLES {
            let pwr_cont1_vin = self.pwr_cont1.read_vin().unwrap_or_else(|code| {
                ringbuf_entry!(Trace::Error(Regulator::PwrCont1, code.into()));
                units::Volts(f32::NAN)
            });

            let pwr_cont2_vin = match self.pwr_cont2.read_vin().unwrap_or_else(|code| {
                ringbuf_entry!(Trace::Error(Regulator::PwrCont2, code.into()));
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
                pwr_cont1_vin,
                pwr_cont2_vin,
            });
        }
    }
}
