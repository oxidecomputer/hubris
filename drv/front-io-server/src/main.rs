// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Front IO board

#![no_std]
#![no_main]

use core::convert::Infallible;
use drv_front_io_api::{
    FrontIOError, LedState,
    transceivers::{
        LogicalPort, LogicalPortMask, ModuleResult, ModuleResultNoFailure,
        PortI2CStatus, TransceiverStatus,
    },
};
use drv_sidecar_seq_api::Sequencer;
use idol_runtime::{Leased, NotificationHandler, R, RequestError, W};
use ringbuf::*;
use userlib::*;

task_slot!(SEQ, sequencer);

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
}
ringbuf!(Trace, 32, Trace::None);

struct ServerImpl {
    seq: Sequencer,
}

impl idl::InOrderFrontIOImpl for ServerImpl {
    /// Returns true if a front IO board was determined to be present and powered on
    fn board_present(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.seq.front_io_board_present())
    }

    /// Returns if the front IO board has completely sequenced and is ready
    fn board_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<FrontIOError>> {
        self.seq
            .front_io_board_ready()
            .map_err(|_| FrontIOError::SeqError)
            .map_err(RequestError::from)
    }

    fn phy_reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.seq
            .reset_front_io_phy()
            .map_err(|_| FrontIOError::SeqError)
            .map_err(RequestError::from)
    }

    /// Set the internal state of the PHY's oscillator
    fn phy_set_osc_state(
        &mut self,
        _: &RecvMessage,
        good: bool,
    ) -> Result<(), RequestError<FrontIOError>> {
       self.seq
            .set_front_io_phy_osc_state(good)
            .map_err(|_| FrontIOError::SeqError)
            .map_err(RequestError::from)
    }

    /// Perform a read from the PHY
    fn phy_read(
        &mut self,
        _: &RecvMessage,
        _phy: u8,
        _reg: u8,
    ) -> Result<u16, RequestError<FrontIOError>> {
        todo!();
    }

    /// Perform a write to the PHY
    fn phy_write(
        &mut self,
        _: &RecvMessage,
        _phy: u8,
        _reg: u8,
        _value: u16,
    ) -> Result<(), RequestError<FrontIOError>> {
        todo!();
    }

    /// Apply reset to the LED controller
    ///
    /// Per section 7.6 of the datasheet the minimum required pulse width here
    /// is 2.5 microseconds. Given the SPI interface runs at 3MHz, the
    /// transaction to clear the reset would take ~10 microseconds on its own,
    /// so there is no additional delay here.
    fn leds_assert_reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        todo!();
    }

    /// Remove reset from the LED controller
    ///
    /// Per section 7.6 of the datasheet the device has a maximum wait time of
    /// 1.5 milliseconds after the release of reset to normal operation, so
    /// there is a 2 millisecond wait here.
    fn leds_deassert_reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        todo!();
    }

    /// Releases the LED controller from reset and enables the output
    fn leds_enable(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        todo!();
    }

    /// Asserts the LED controller reset and disables the output
    fn leds_disable(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        todo!();
    }

    /// Update the internal port LED state of each bit in `mask` to `state`
    fn led_set_state(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
        _state: LedState,
    ) -> Result<(), RequestError<Infallible>> {
        todo!();
    }

    /// Return the LED state of each port
    fn led_get_state(
        &mut self,
        _: &RecvMessage,
        _port: LogicalPort,
    ) -> Result<LedState, RequestError<Infallible>> {
        todo!();
    }

    /// Return the LED state of the system LED
    fn led_get_system_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<LedState, RequestError<Infallible>> {
        todo!();
    }

    /// Turn the system LED on
    fn led_set_system_on(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        todo!();
    }

    /// Turn the system LED off
    fn led_set_system_off(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        todo!();
    }

    /// Blink the system LED
    fn led_set_system_blink(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        todo!();
    }

    /// Get the current status of all modules
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// TransceiverStatus to be consumed or ignored by the caller.
    fn transceivers_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<TransceiverStatus, RequestError<Infallible>> {
        todo!();
    }

    /// Enable power for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_enable_power(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        todo!();
    }

    /// Disable power for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_disable_power(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        todo!();
    }

    /// Clear a fault for each port per the specified `mask`.
    ///
    /// The meaning of the
    /// returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_clear_power_fault(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        todo!();
    }

    /// Assert reset for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_assert_reset(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        todo!();
    }

    /// Deassert reset for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_deassert_reset(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        todo!();
    }

    /// Assert LpMode for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_assert_lpmode(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        todo!();
    }

    /// Deassert LpMode for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_deassert_lpmode(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        todo!();
    }

    /// Initiate an I2C random read on all ports per the specified `mask`.
    ///
    /// The maximum value of `num_bytes` is 128. This operation is considered
    /// infallible because the error cases are handled by the transceivers
    /// crate, which then passes back a ModuleResultNoFailure to be consumed or
    /// ignored by the caller. The meaning of the returned
    /// `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_setup_i2c_read(
        &mut self,
        _: &RecvMessage,
        _reg: u8,
        _num_bytes: u8,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<FrontIOError>> {
        todo!();
    }

    /// Initiate an I2C write on all ports per the specified `mask`.
    ///
    /// The maximum value of `num_bytes` is 128. This operation is considered
    /// infallible because the error cases are handled by the transceivers
    /// crate, which then passes back a ModuleResultNoFailure to be consumed or
    /// ignored by the caller. The meaning of the returned
    /// `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_setup_i2c_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        _reg: u8,
        _num_bytes: u8,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<FrontIOError>> {
        todo!();
    }

    /// Write `data` into the I2C write buffer for each port specified by `mask`
    ///
    /// The maximum value of `num_bytes` is 128. This operation is considered
    /// infallible because the error cases are handled by the transceivers
    /// crate, which then passes back a ModuleResultNoFailure to be consumed or
    /// ignored by the caller. The meaning of the returned
    /// `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_set_i2c_write_buffer(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
        _data: Leased<R, [u8]>,
    ) -> Result<ModuleResultNoFailure, RequestError<FrontIOError>> {
        todo!();
    }

    /// Get both the status byte and the read data buffer for the specified `port`
    fn transceivers_get_i2c_status_and_read_buffer(
        &mut self,
        _: &RecvMessage,
        _port: LogicalPort,
        _dest: Leased<W, [u8]>,
    ) -> Result<PortI2CStatus, RequestError<FrontIOError>> {
        todo!();
    }

    fn transceivers_wait_and_check_i2c(
        &mut self,
        _: &RecvMessage,
        _mask: LogicalPortMask,
    ) -> Result<ModuleResult, RequestError<Infallible>> {
        todo!();
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: NotificationBits) {}
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    let mut server = ServerImpl {
        seq: Sequencer::from(SEQ.get_task_id()),
    };

    // This will put our timer in the past, and should immediately kick us.
    let deadline = sys_get_timer().now;
    sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{
        FrontIOError, LedState, LogicalPort, LogicalPortMask, ModuleResult,
        ModuleResultNoFailure, PortI2CStatus, TransceiverStatus,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
