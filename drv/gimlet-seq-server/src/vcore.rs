// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

///
/// We have seen adventures on the V12_SYS_A2 rail in that it will sag from
/// 12V to ~8V over a period of about ~4ms, and then rise back 12V over ~7ms.
/// This happens only on very few machines, and even then happens very rarely
/// (happening once over hours or days), but the consequences are acute:  the
/// dip in power results in U.2 drives resetting, and ultimately, the system
/// resetting itself.  To better characterize any such dips, we want to use
/// one of the rails on of the RAA229618s (specifically, VDD_VCORE) as a
/// witness to any V12_SYS_A2 rail fluctuation via its VIN: we set its VIN
/// undervoltage warning limit to a value that is lower than any we expect in
/// an operable system (but higher than the sags we have observed), and then
/// configure its fault output (PWR_CONT1_VCORE_TO_SP_ALERT_L, connected to
/// PI14) to generate an interrupt on a falling edge.  Upon the interrupt, we
/// will get notification here, and we will record values of VIN as quickly as
/// we can.  Each READ_VIN requires 8 bytes, over 3 I2C transactions:
///
///   [Write + PAGE + rail] [Write + READ_VIN] [Read + MSB + LSB]
///
/// At our midbus speed of 100kHz, this is ~900µs per READ_VIN.  We gather 50
/// of these READ_VIN measurements, along with timestamps before and after the
/// operations, and put them all in a ring buffer.  Note that we don't clear
/// faults after this condition; we will wait until the machine next makes an
/// A2 to A0 transition to clear faults.
///
use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_i2c_devices::raa229618::Raa229618;
use drv_stm32xx_sys_api as sys_api;
use ringbuf::*;
use sys_api::{gpio_irq_pins::VCORE_TO_SP_ALERT_L, IrqControl};
use userlib::*;

pub struct VCore {
    device: Raa229618,
    sys: sys_api::Sys,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Initializing,
    Initialized,
    LimitLoaded,
    FaultsCleared,
    Notified,
    Fault,
    Reading { timestamp: u64, volts: units::Volts },
    Error(ResponseCode),
    None,
}

ringbuf!(Trace, 120, Trace::None);

///
/// We are going to set our input undervoltage warn limit to be 11.75 volts.
/// Note that we will not fault if VIN goes below this (that is, we will not
/// lose POWER_GOOD), but the part will indicate an input fault and pull
/// PWR_CONT1_VCORE_TO_SP_ALERT_L low.
///
const VCORE_UV_WARN_LIMIT: units::Volts = units::Volts(11.75);

///
/// We want to collect enough samples (at ~900µs per sample) to adequately
/// cover any anticipated dip.  We have seen these have an ~11ms total width
/// in the wild, so we give ourselves plenty of margin here and get ~45ms
/// of data.
///
const VCORE_NSAMPLES: usize = 50;

cfg_if::cfg_if! {
    if #[cfg(not(any(
        target_board = "gimlet-b",
        target_board = "gimlet-c",
        target_board = "gimlet-d",
        target_board = "gimlet-e",
        target_board = "gimlet-f",
    )))] {
        compile_error!("RAA229618 VIN monitoring unsupported for this board");
    }
}

impl VCore {
    pub fn new(sys: &sys_api::Sys, device: &I2cDevice, rail: u8) -> Self {
        Self {
            device: Raa229618::new(device, rail),
            sys: sys.clone(),
        }
    }

    pub fn mask(&self) -> u32 {
        crate::notifications::VCORE_MASK
    }

    pub fn initialize_uv_warning(&self) -> Result<(), ResponseCode> {
        let sys = &self.sys;

        ringbuf_entry!(Trace::Initializing);

        // Set our warn limit
        self.device.set_vin_uv_warn_limit(VCORE_UV_WARN_LIMIT)?;
        ringbuf_entry!(Trace::LimitLoaded);

        // Clear our faults
        self.device.clear_faults()?;
        ringbuf_entry!(Trace::FaultsCleared);

        // Set our alert line to be an input
        sys.gpio_configure_input(VCORE_TO_SP_ALERT_L, sys_api::Pull::None);
        sys.gpio_irq_configure(self.mask(), sys_api::Edge::Falling);

        // Enable the interrupt!
        let _ = self.sys.gpio_irq_control(self.mask(), IrqControl::Enable);

        ringbuf_entry!(Trace::Initialized);

        Ok(())
    }

    pub fn handle_notification(&self) {
        let faulted = self.sys.gpio_read(VCORE_TO_SP_ALERT_L) == 0;

        ringbuf_entry!(Trace::Notified);

        if faulted {
            ringbuf_entry!(Trace::Fault);

            for _ in 0..VCORE_NSAMPLES {
                match self.device.read_vin() {
                    Ok(val) => {
                        //
                        // Record our reading, along with a timestamp.  On the
                        // one hand, it's a little exceesive to record a
                        // timestamp on every reading:  it's in milliseconds,
                        // and because it takes ~900µs per reading, we expect
                        // the timestamp to (basically) be incremented by 1 with
                        // every reading (with a duplicate timestamp occuring
                        // every ~7-9 entries).  But on the other, it's not
                        // impossible to be preempted, and it's valuable to have
                        // as tight a coupling as possible between observed
                        // reading and observed time.
                        //
                        ringbuf_entry!(Trace::Reading {
                            timestamp: sys_get_timer().now,
                            volts: val,
                        });
                    }
                    Err(code) => ringbuf_entry!(Trace::Error(code.into())),
                }
            }
        }

        let _ = self.sys.gpio_irq_control(self.mask(), IrqControl::Enable);
    }
}
