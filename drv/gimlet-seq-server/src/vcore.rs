// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{retry_i2c_txn, I2cTxn};
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
use crate::gpio_irq_pins::VCORE_TO_SP_ALERT_L;
use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_i2c_devices::raa229618::Raa229618;
use drv_stm32xx_sys_api as sys_api;
use fixedstr::FixedStr;
use ringbuf::*;
use sys_api::IrqControl;
use task_packrat_api as packrat_api;
use userlib::{sys_get_timer, units};

pub struct VCore {
    device: Raa229618,
    sys: sys_api::Sys,
}

#[derive(microcbor::EncodeFields)]
pub(super) struct PmbusEreport {
    refdes: fixedstr::FixedStr<{ crate::i2c_config::MAX_COMPONENT_ID_LEN }>,
    rail: &'static fixedstr::FixedStr<10>,
    time: u64,
    pwr_good: Option<bool>,
    pmbus_status: PmbusStatus,
}

#[derive(Copy, Clone, Default, microcbor::Encode)]
struct PmbusStatus {
    word: Option<u16>,
    input: Option<u8>,
    iout: Option<u8>,
    vout: Option<u8>,
    temp: Option<u8>,
    cml: Option<u8>,
    mfr: Option<u8>,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Initializing,
    Initialized,
    LimitLoaded,
    FaultsCleared,
    Notified { timestamp: u64, asserted: bool },
    RegulatorStatus { power_good: bool, faulted: bool },
    StatusWord(Result<u16, ResponseCode>),
    StatusInput(Result<u8, ResponseCode>),
    StatusVout(Result<u8, ResponseCode>),
    StatusIout(Result<u8, ResponseCode>),
    StatusTemperature(Result<u8, ResponseCode>),
    StatusCml(Result<u8, ResponseCode>),
    StatusMfrSpecific(Result<u8, ResponseCode>),
    Reading { timestamp: u64, volts: units::Volts },
    Error(ResponseCode),
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

    pub fn handle_notification(
        &self,
        packrat: &packrat_api::Packrat,
        ereport_buf: &mut [u8; crate::EREPORT_BUF_LEN],
    ) {
        let now = sys_get_timer().now;
        let asserted = self.sys.gpio_read(VCORE_TO_SP_ALERT_L) == 0;

        ringbuf_entry!(Trace::Notified {
            timestamp: now,
            asserted
        });

        if asserted {
            self.read_pmbus_status(now, packrat, ereport_buf);
            // Clear the fault now so that PMALERT_L is reasserted if a
            // subsequent fault occurs. Note that if the fault *condition*
            // continues, the fault bits in the status registers will remain
            // set, and sending the CLEAR_FAULTS command does *not* cause the
            // device to power back on if it's off.
            let _ = self.device.clear_faults();
            ringbuf_entry!(Trace::FaultsCleared);
        }

        let _ = self.sys.gpio_irq_control(self.mask(), IrqControl::Enable);
    }

    fn read_pmbus_status(
        &self,
        now: u64,
        packrat: &packrat_api::Packrat,
        ereport_buf: &mut [u8],
        ereport_buf: &mut [u8; crate::EREPORT_BUF_LEN],
    ) {
        use pmbus::commands::raa229618::STATUS_WORD;

        // Read PMBus status registers and prepare an ereport.
        let status_word = retry_i2c_txn(I2cTxn::VCorePmbusStatus, || {
            self.device.status_word()
        });
        ringbuf_entry!(Trace::StatusWord(status_word.map(|s| s.0)));

        let mut input_fault = false;
        let pwr_good = if let Ok(status) = status_word {
            // If any fault bits are hot, set this VRM to "faulted", even if it
            // was not the one whose `PMALERT` assertion actually triggered our
            // IRQ.
            //
            // Note: since these are all single bits in the PMBus STATUS_WORD,
            // the PMBus crate *should* never return `None` for them, as there
            // are no un-interpretable values possible. Either a bit is set or
            // it is not.
            let mut faulted = false;
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
                power_good,
                faulted
            });

            // If we haven't faulted, and POWER_GOOD is asserted, nothing left
            // to do here.
            if !faulted && power_good {
                return;
            }
            Some(power_good)
        } else {
            None
        };

        // Read remaining status registers.
        let status_input = retry_i2c_txn(I2cTxn::VCorePmbusStatus, || {
            self.device.status_input()
        })
        .map(|s| s.0);
        ringbuf_entry!(Trace::StatusInput(status_input));
        let status_vout = retry_i2c_txn(I2cTxn::VCorePmbusStatus, || {
            self.device.status_vout()
        })
        .map(|s| s.0);
        ringbuf_entry!(Trace::StatusVout(status_vout));
        let status_iout = retry_i2c_txn(I2cTxn::VCorePmbusStatus, || {
            self.device.status_iout()
        })
        .map(|s| s.0);
        ringbuf_entry!(Trace::StatusIout(status_iout));
        let status_temperature =
            retry_i2c_txn(I2cTxn::VCorePmbusStatus, || {
                self.device.status_temperature()
            })
            .map(|s| s.0);
        ringbuf_entry!(Trace::StatusTemperature(status_temperature));
        let status_cml = retry_i2c_txn(I2cTxn::VCorePmbusStatus, || {
            self.device.status_cml()
        })
        .map(|s| s.0);
        ringbuf_entry!(Trace::StatusCml(status_cml));
        let status_mfr_specific =
            retry_i2c_txn(I2cTxn::VCorePmbusStatus, || {
                self.device.status_mfr_specific()
            })
            .map(|s| s.0);
        ringbuf_entry!(Trace::StatusMfrSpecific(status_mfr_specific));

        let status = super::PmbusStatus {
            word: status_word.map(|s| s.0).ok(),
            input: status_input.ok(),
            vout: status_vout.ok(),
            iout: status_iout.ok(),
            temp: status_temperature.ok(),
            cml: status_cml.ok(),
            mfr: status_mfr_specific.ok(),
        };

        static RAIL: FixedStr<10> = FixedStr::from_str("VDD_VCORE");
        let ereport = packrat_api::Ereport {
            class: crate::EreportClass::PmbusAlert,
            version: 0,
            report: crate::EreportKind::PmbusAlert {
                refdes: FixedStr::from_str(
                    self.device.i2c_device().component_id(),
                ),
                rail: &RAIL,
                time: now,
                pwr_good,
                pmbus_status: status,
            },
        };

        // TODO(eliza): if POWER_GOOD has been deasserted, we should produce a
        // subsequent ereport for that.

        // If the `INPUT_FAULT` bit in `STATUS_WORD` is set, or any bit is hot
        // in `STATUS_INPUT`, sample Vin in order to record the voltage dip in
        // the ringbuf. If we weren't able to read these status registers, let's
        // also go ahead and record the input voltage, just in case.
        if input_fault || status_input != Ok(0) {
            // "Houston, we've got a main bus B undervolt..."
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
    }
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

#[derive(Serialize)]
struct PmbusEreport {
    #[serde(rename = "k")]
    class: crate::EreportClass,
    #[serde(rename = "v")]
    version: u32,
    refdes: &'static str,
    rail: &'static str,
    time: u64,
    pwr_good: Option<bool>,
    pmbus_status: PmbusStatus,
}
