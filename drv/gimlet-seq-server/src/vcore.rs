// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

///
/// We have seen adventures on the V12_SYS_A2 rail in that it will (on some
/// machines and for unclear reason) very rarely droop from 12V to ~8V over a
/// period of about ~4ms, and then rise back 12V over ~7ms. (This dip in power
/// results in U.2 drives resetting, and ultimately, the system resetting
/// itself.)  To understand these dips we are using one of the rails on of the
/// RAA229618s (specifically, VDD_VCORE) as a witness to any V12_SYS_A2 rail
/// fluctuation via its VIN: we set its VIN undervoltage warning limit to a
/// value that is lower than any we expect in an operable system (but higher
/// than the droops we have observed), and then setup its fault output
/// (PWR_CONT1_VCORE_TO_SP_ALERT_L, on PI14) to generate an interrupt on a
/// falling edge.  Upon the interrupt, we will get notification here, and we
/// will record values of VIN as quickly as we can.  This is as fast as I2C,
/// which necessitates 8 bytes per READ_VIN:
///
///   [Write PAGE rail] [Write READ_VIN] [Read MSB LSB]
///
/// At 100kHz, this is ~900µs per READ_VIN.  We gather 50 of these READ_VIN
/// measurements, along with timestamps before and after the operations, and
/// put them all in a ring buffer.  Note that we don't clear faults after this
/// condition; we will wait until the machine next makes an A2 to A0
/// transition to clear faults.
///
///
use drv_i2c_api::I2cDevice;
use drv_i2c_devices::raa229618::Raa229618;
use drv_stm32xx_sys_api as sys_api;
use ringbuf::*;
use userlib::*;

use crate::notifications;

pub struct VCore {
    device: Raa229618,
    sys: sys_api::Sys,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Notified,
    Fault,
    Start(u64),
    Reading(units::Volts),
    Error(drv_i2c_api::ResponseCode),
    Done(u64),
    None,
}

ringbuf!(Trace, 120, Trace::None);

const VCORE_NSAMPLES: usize = 50;
const VCORE_TO_SP_ALERT_L: sys_api::PinSet = sys_api::Port::I.pin(14);
const VCORE_TO_SP_ALERT_PULL: sys_api::Pull = sys_api::Pull::None;

impl VCore {
    pub fn new(sys: &sys_api::Sys, device: &I2cDevice, rail: u8) -> Self {
        Self {
            device: Raa229618::new(&device, rail),
            sys: sys.clone(),
        }
    }

    pub fn mask(&self) -> u32 {
        crate::notifications::VCORE_MASK
    }

    pub fn initialize_uv_warning(&self) {
        //
        // We are going to set our input undervoltage warn limit to be 11.75
        // volts.  We definitely don't expect the line to droop that far --
        // and if it does, we assume that we are interested in collecting our
        // VIN samples.
        //
        self.device.set_vin_uv_warn_limit(units::Volts(11.75));

        // Clear our faults
        self.device.clear_faults();

        // Set our alert line to be an input
        self.sys
            .gpio_configure_input(VCORE_TO_SP_ALERT_L, VCORE_TO_SP_ALERT_PULL);
        self.sys
            .gpio_irq_configure(self.mask(), sys_api::Edge::Falling);

        self.sys.gpio_irq_control(0, self.mask());
    }

    pub fn handle_notification(&self) {
        let faulted = self.sys.gpio_read(VCORE_TO_SP_ALERT_L) == 0;

        ringbuf_entry!(Trace::Notified);

        if !faulted {
            self.sys.gpio_irq_control(0, notifications::VCORE_MASK);
            return;
        }

        ringbuf_entry!(Trace::Fault);
        ringbuf_entry!(Trace::Start(sys_get_timer().now));

        for _ in 0..VCORE_NSAMPLES {
            match self.device.read_vin() {
                Ok(val) => ringbuf_entry!(Trace::Reading(val)),
                Err(code) => ringbuf_entry!(Trace::Error(code.into())),
            }
        }

        ringbuf_entry!(Trace::Done(sys_get_timer().now));
        self.sys.gpio_irq_control(0, notifications::VCORE_MASK);
    }
}
